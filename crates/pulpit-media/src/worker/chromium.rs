//! The external Chromium HTML worker (`docs-src/internals.typ`).
//!
//! Ships no browser engine. Discovers an installed Chromium-family browser,
//! launches it headless with a private profile, drives it over an inherited
//! debugging pipe, and turns CDP screencast frames into ordinary surface
//! frames. The user's own profile, extensions and logins stay unreachable.
//!
//! stdout is the protocol stream, so nothing else may ever be printed there.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pulpit_core::overlay::ContentKind;

use crate::protocol::{
    read_message, write_message, InputEvent, MediaError, MediaErrorKind, MediaEvent, MediaRequest,
    PixelFormat, PointerButton, RuntimeId, SessionId, SessionSource, SessionSpec, SessionState,
    SurfaceFrame, Viewport, WebCommand, WorkerCounters,
};
use crate::runtime::chromium::{wrapper_page, AssetServer, CdpPipe, Json, WrapperPlayback};
use crate::runtime::scale::fit_rgba;
use crate::runtime::{discover_chromium, probe_external_chromium};
use crate::surface::AttachedRing;
use crate::worker::reply;

/// How long one command may take once the browser is up and a page is on
/// screen.
///
/// Deliberately short. Past this the browser is not slow, it is wedged, and a
/// presentation is the worst place to find that out slowly.
const COMMAND_DEADLINE: Duration = Duration::from_secs(10);

/// How long the commands that *bring* a browser and a page up may take.
///
/// Three times [`COMMAND_DEADLINE`], because cold start is a different
/// activity from steady state and was being measured against the wrong one. A
/// first command's reply waits on process start, profile creation, GPU and
/// renderer init, and the first paint of a page — seconds of real work on an
/// idle machine, and on a loaded one (a CI runner, a laptop compiling
/// something) several times that. Ten seconds was enough on an idle machine
/// and not enough on a busy one, which is a timeout that fails exactly when
/// the user is least able to do anything about it.
///
/// Under the 40-second ceiling the media tests give a session to produce its
/// first frames, so a genuine hang is reported here, by the command that hung,
/// rather than as a test that saw no frames and cannot say why.
const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
const IDLE_TICK: Duration = Duration::from_millis(4);
/// The loop cadence while every session is parked: nothing is producing
/// frames, so only stdin commands and stray CDP events need service.
const PARKED_TICK: Duration = Duration::from_millis(50);

/// The function a wrapper page calls to say where playback has reached. It is
/// installed as a CDP binding, so this name and the one in the page must
/// match exactly.
const PROGRESS_BINDING: &str = "__tpReport";

/// The object a wrapper page exposes for the host to drive it through.
const CONTROL_OBJECT: &str = "window.__tp";

/// One browser process, shared by every session this worker hosts.
///
/// A browser start costs hundreds of milliseconds and a hundred megabytes, and
/// a deck with four overlays on a slide used to pay that four times over. The
/// engine is the same for all of them; only the *page* differs. So the process
/// is opened once and each session gets a page inside it.
///
/// Sharing one pipe means one reader. Every message the browser sends carries
/// the `sessionId` of the page it belongs to, so reading is centralised here
/// and events are filed per page for their own session to collect. A session
/// that read the pipe itself would swallow another session's frames.
struct Browser {
    pipe: CdpPipe,
    /// Events waiting for the page they belong to, keyed by CDP session id.
    inbox: std::collections::HashMap<String, std::collections::VecDeque<Json>>,
    /// Pages that have been prepared before and are idle again. Reusing one
    /// is cheaper than creating a target, and keeps the browser from ever
    /// reaching zero pages — closing the last one can end the process.
    free: Vec<Page>,
    /// True once the browser's own initial page has been handed out.
    initial_claimed: bool,
}

/// A page inside the shared browser: the target that owns it, and the CDP
/// session every `Page`, `Input`, `Runtime`, `Fetch` and `Emulation` command
/// for it must carry.
#[derive(Clone, Debug)]
struct Page {
    target: String,
    session: String,
    /// The browser's own first page, which is reused rather than closed.
    initial: bool,
}

/// The `Emulation.setDeviceMetricsOverride` parameters.
///
/// Sent when a page is prepared and again whenever its viewport changes, and
/// the two must agree: a resize that described the page differently from the
/// way it was set up would move the content rather than resize it.
fn device_metrics(css_width: u32, css_height: u32, scale: f32) -> Json {
    serde_json::json!({
        "width": css_width,
        "height": css_height,
        "deviceScaleFactor": scale,
        "mobile": false,
    })
}

impl Browser {
    fn launch(executable: &Path, viewport: Viewport) -> Result<Self, MediaError> {
        // A fresh profile per worker lifetime, removed when the pipe drops.
        let token = crate::runtime::chromium::unguessable_token()?;
        let profile =
            std::env::temp_dir().join(format!("pulpit-browser-{}-{}", std::process::id(), token));
        std::fs::create_dir_all(&profile).map_err(|e| {
            MediaError::new(
                MediaErrorKind::LaunchFailed,
                format!("could not create a private browser profile: {e}"),
            )
        })?;
        let mut pipe = CdpPipe::launch(executable, &profile, viewport, &[])?;
        // Version and feature probing happen before any document content is
        // loaded, so an incompatible browser fails here rather than on stage.
        let product = pipe.feature_probe(STARTUP_DEADLINE)?;
        tracing::debug!(%product, "browser ready");
        Ok(Self {
            pipe,
            inbox: std::collections::HashMap::new(),
            free: Vec::new(),
            initial_claimed: false,
        })
    }

    /// File one message under the page it belongs to.
    fn route(&mut self, message: Json) {
        let Some(session) = message.get("sessionId").and_then(Json::as_str) else {
            return;
        };
        // Checked before the entry API so the session id String is only
        // allocated for a genuinely new session, not per routed message.
        if !self.inbox.contains_key(session) {
            self.inbox.insert(session.to_string(), Default::default());
        }
        let queue = self.inbox.get_mut(session).expect("just ensured");
        // A session whose consumer has stopped collecting must not grow an
        // unbounded queue; the oldest events are the stale ones.
        const MAX_QUEUED: usize = 512;
        if queue.len() >= MAX_QUEUED {
            queue.pop_front();
        }
        queue.push_back(message);
    }

    /// Read whatever has arrived, filing it for its page. Returns after
    /// `deadline` with nothing to read, which is the idle case.
    fn poll(&mut self, deadline: Duration) -> Result<(), MediaError> {
        // A bounded drain rather than a single read: with several overlays
        // live, one message per loop iteration would fall behind the browser.
        const MAX_PER_POLL: usize = 256;
        for index in 0..MAX_PER_POLL {
            // Only the first read waits; the rest take what is already here.
            let wait = if index == 0 { deadline } else { Duration::ZERO };
            match self.pipe.recv(wait) {
                Ok(message) => self.route(message),
                Err(error) if error.kind == MediaErrorKind::TimedOut => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Whether anything is filed for one page. What `take` would return,
    /// without taking it — the idle loop's cheap "does this session need a
    /// pump at all" question.
    fn has_events(&self, page: &str) -> bool {
        self.inbox.get(page).is_some_and(|queue| !queue.is_empty())
    }

    /// Everything filed for one page since it last collected.
    fn take(&mut self, page: &str) -> std::collections::VecDeque<Json> {
        self.inbox
            .get_mut(page)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Wait for one command's reply, filing every event that arrives while
    /// waiting so another page's frames are not lost to this command.
    fn wait_for(&mut self, id: u64, deadline: Duration) -> Result<Json, MediaError> {
        let mut stray = Vec::new();
        let result = self.pipe.wait_for(id, deadline, |event| {
            stray.push(event);
        });
        for event in stray {
            self.route(event);
        }
        result
    }

    /// Send a command to one page and wait for its reply.
    fn command(&mut self, page: &str, method: &str, params: Json) -> Result<Json, MediaError> {
        self.command_within(page, method, params, COMMAND_DEADLINE)
    }

    /// The same, for a caller that knows this command is not steady state —
    /// see [`STARTUP_DEADLINE`].
    fn command_within(
        &mut self,
        page: &str,
        method: &str,
        params: Json,
        deadline: Duration,
    ) -> Result<Json, MediaError> {
        let id = self.pipe.send_to_session(method, params, Some(page))?;
        self.wait_for(id, deadline)
    }

    /// Send a command to one page without waiting.
    fn send(&mut self, page: &str, method: &str, params: Json) -> Result<u64, MediaError> {
        self.pipe.send_to_session(method, params, Some(page))
    }

    /// A page for a new session: an idle one if there is one, otherwise the
    /// browser's own initial page, otherwise a freshly created window.
    fn open_page(&mut self) -> Result<Page, MediaError> {
        if let Some(page) = self.free.pop() {
            return Ok(page);
        }
        if !self.initial_claimed {
            if let Some(page) = self.initial_page()? {
                self.initial_claimed = true;
                return Ok(page);
            }
        }
        // `newWindow` matters: a target created as a background *tab* is not
        // rendered, and an unrendered page produces no screencast frames at
        // all. Its own window is active from the moment it exists.
        let created = self.pipe.send(
            "Target.createTarget",
            serde_json::json!({ "url": "about:blank", "newWindow": true, "background": false }),
        )?;
        let created = self.wait_for(created, STARTUP_DEADLINE)?;
        let target = created
            .get("targetId")
            .and_then(Json::as_str)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorKind::Incompatible,
                    "the browser opened no page for this overlay",
                )
            })?
            .to_string();
        let session = self.attach(&target)?;
        Ok(Page {
            target,
            session,
            initial: false,
        })
    }

    /// The page the browser opened for itself, if it still has one.
    ///
    /// It is preferred over a created target for the first session because it
    /// is the path this adapter has always used, and it costs nothing.
    fn initial_page(&mut self) -> Result<Option<Page>, MediaError> {
        let id = self.pipe.send("Target.getTargets", serde_json::json!({}))?;
        let listed = self.wait_for(id, STARTUP_DEADLINE)?;
        let Some(target) = listed
            .get("targetInfos")
            .and_then(Json::as_array)
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|target| target.get("type").and_then(Json::as_str) == Some("page"))
            })
            .and_then(|target| target.get("targetId"))
            .and_then(Json::as_str)
            .map(str::to_string)
        else {
            return Ok(None);
        };
        let session = self.attach(&target)?;
        Ok(Some(Page {
            target,
            session,
            initial: true,
        }))
    }

    /// Attach to a target and return its CDP session id.
    ///
    /// Without this every `Page.*`, `Input.*` and `Emulation.*` call goes to
    /// the *browser* target, which has none of those domains and answers
    /// "'Page.enable' wasn't found". `Browser.getVersion` does work there,
    /// which is exactly why the capability probe passed while nothing ever
    /// rendered.
    fn attach(&mut self, target: &str) -> Result<String, MediaError> {
        // `flatten` puts the page's messages on this same pipe, tagged with a
        // session id, rather than wrapping them in Target.receivedMessage.
        let id = self.pipe.send(
            "Target.attachToTarget",
            serde_json::json!({ "targetId": target, "flatten": true }),
        )?;
        let attached = self.wait_for(id, STARTUP_DEADLINE)?;
        attached
            .get("sessionId")
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorKind::Incompatible,
                    "the browser would not attach to a page",
                )
            })
    }

    /// Return a page to the pool once its session is over.
    ///
    /// The deck's document is left behind — screencast stopped, interception
    /// dropped, navigated away — so a reused page carries none of it. A page
    /// that will not come back cleanly is closed instead of being reused.
    fn release_page(&mut self, page: Page) {
        let quiesced = self
            .command(&page.session, "Page.stopScreencast", serde_json::json!({}))
            .and_then(|_| self.command(&page.session, "Fetch.disable", serde_json::json!({})))
            .and_then(|_| {
                self.command(
                    &page.session,
                    "Page.navigate",
                    serde_json::json!({ "url": "about:blank" }),
                )
            });
        self.inbox.remove(&page.session);
        match quiesced {
            Ok(_) => self.free.push(page),
            Err(error) => {
                tracing::debug!(%error, "closing a page that would not quiesce");
                if !page.initial {
                    let _ = self.pipe.send(
                        "Target.closeTarget",
                        serde_json::json!({ "targetId": page.target }),
                    );
                }
            }
        }
    }
}

struct Session {
    spec: SessionSpec,
    /// Kept alive for the session: dropping it stops serving the bundle.
    _server: AssetServer,
    ring: AttachedRing,
    page: Page,
    /// The frame size last asked of the screencast, so a viewport change
    /// restarts it only when the size actually changed.
    cast_size: (u32, u32),
    /// Whether a screencast is currently running on the page. False while the
    /// session is inactive: a page nobody sees must not keep the browser
    /// encoding JPEG frames for it.
    cast_running: bool,
    sequence: u64,
    active: bool,
    /// The earliest moment the next frame is worth decoding, when a publish
    /// rate cap is set. Frames arriving sooner are acknowledged and dropped
    /// before their decode is paid.
    next_publish: Option<Instant>,
    /// Seconds-per-frame form of `spec.max_fps`; `None` means uncapped.
    publish_interval: Option<Duration>,
    /// Scratch for the base64-decoded JPEG, reused across frames.
    jpeg: Vec<u8>,
    /// Scratch for RGB→RGBA expansion, reused across frames.
    rgba_scratch: Vec<u8>,
    pointer: (f32, f32),
}

impl Session {
    fn open(spec: SessionSpec, browser: &mut Browser) -> Result<Self, MediaError> {
        spec.validate()
            .map_err(|e| MediaError::new(MediaErrorKind::ProtocolViolation, e.to_string()))?;
        // A bundle brings its own document; a bare file gets one generated
        // around it. That is the whole difference between playing HTML and
        // playing a GIF or a clip — the browser decodes all three.
        let (root, entrypoint, generated) = match spec.source.clone() {
            SessionSource::Bundle { root, entrypoint } => (PathBuf::from(root), entrypoint, None),
            SessionSource::File { path } => {
                let file = PathBuf::from(&path);
                let name = file
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorKind::MalformedAsset,
                            "that media path names no file",
                        )
                    })?;
                let directory = file
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| file.clone());
                let page = wrapper_page(
                    &name,
                    spec.kind == ContentKind::Video,
                    &WrapperPlayback {
                        autoplay: spec.playback.autoplay,
                        repeat: spec.playback.repeat,
                        mute: spec.playback.mute,
                        start: spec.playback.start,
                    },
                );
                (
                    directory,
                    AssetServer::GENERATED_PAGE.to_string(),
                    Some(page),
                )
            }
        };

        // Only the staged files are servable, and only through the private
        // origin: the media never sees an unrestricted `file://` root.
        let allowlist = collect_files(&root)?;
        let server = AssetServer::start_with_page(root, allowlist, generated)?;

        let ring = AttachedRing::attach(&spec.ring_name, spec.slots, spec.slot_bytes)
            .map_err(|e| MediaError::new(MediaErrorKind::ResourceLimit, e.to_string()))?;

        let page = browser.open_page()?;
        let publish_interval =
            (spec.max_fps > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(spec.max_fps)));
        let mut session = Self {
            cast_size: (spec.viewport.width, spec.viewport.height),
            spec,
            _server: server,
            ring,
            page,
            cast_running: false,
            sequence: 0,
            active: true,
            next_publish: None,
            publish_interval,
            jpeg: Vec::new(),
            rgba_scratch: Vec::new(),
            pointer: (0.0, 0.0),
        };
        if let Err(error) = session.prepare(browser, &entrypoint) {
            browser.release_page(session.page.clone());
            return Err(error);
        }
        Ok(session)
    }

    /// Send a command to this session's page and wait for its reply.
    ///
    /// The page's session id is cloned because it and the browser are borrowed
    /// together; it is a short string and this is not a hot path.
    fn command(
        &mut self,
        browser: &mut Browser,
        method: &str,
        params: Json,
    ) -> Result<Json, MediaError> {
        let page = self.page.session.clone();
        browser.command(&page, method, params)
    }

    /// The same, for the commands that bring this session up — see
    /// [`STARTUP_DEADLINE`].
    fn command_within(
        &mut self,
        browser: &mut Browser,
        method: &str,
        params: Json,
        deadline: Duration,
    ) -> Result<Json, MediaError> {
        let page = self.page.session.clone();
        browser.command_within(&page, method, params, deadline)
    }

    /// Send a command to this session's page without waiting for a reply.
    fn send_page(
        &mut self,
        browser: &mut Browser,
        method: &str,
        params: Json,
    ) -> Result<u64, MediaError> {
        let page = self.page.session.clone();
        browser.send(&page, method, params)
    }

    /// Drive the page through the control object it exposes.
    ///
    /// `call` is built by [`video_call`] and [`image_call`] out of a fixed set
    /// of method names and numbers this worker formatted itself, never out of
    /// anything the application supplied as text. That is what keeps this from
    /// being the generic `eval` the protocol deliberately does not have.
    fn call_control(&mut self, browser: &mut Browser, call: &str) -> Result<u64, MediaError> {
        self.send_page(
            browser,
            "Runtime.evaluate",
            serde_json::json!({
                // A page whose script failed to run still has to not take the
                // worker down with it, so the call is guarded rather than
                // assumed to land.
                "expression": format!("if({CONTROL_OBJECT})({CONTROL_OBJECT}).{call}"),
                "returnByValue": true,
            }),
        )
    }

    fn prepare(&mut self, browser: &mut Browser, entrypoint: &str) -> Result<(), MediaError> {
        let (css_width, css_height) = self.spec.viewport.css_size();
        let url = self._server.url_for(entrypoint);

        for (method, params) in [
            ("Page.enable", serde_json::json!({})),
            ("Runtime.enable", serde_json::json!({})),
            // How the page reports its playhead. Installed before the
            // navigation below, because a binding added afterwards is missing
            // from the context the document actually runs in. It applies to
            // every context created later too, so it survives that navigation
            // and any reload after it.
            (
                "Runtime.addBinding",
                serde_json::json!({ "name": PROGRESS_BINDING }),
            ),
            (
                "Emulation.setDeviceMetricsOverride",
                device_metrics(css_width, css_height, self.spec.viewport.scale),
            ),
        ] {
            // Bring-up: the browser may still be starting behind these.
            self.command_within(browser, method, params, STARTUP_DEADLINE)?;
        }

        // The screencast starts *before* the navigation, and the order is not
        // arbitrary. Navigating to the bundle's origin swaps the page's
        // render frame; the handler this session is bound to then no longer
        // has an active one, and `Page.startScreencast` is refused with "Not
        // attached to an active page". Started first, the screencast is bound
        // to the tab and survives the swap.
        self.start_screencast(browser, STARTUP_DEADLINE)?;

        let url_params = serde_json::json!({ "url": url });
        let id = self.send_page(browser, "Page.navigate", url_params)?;
        // The slowest of the lot: this reply waits on the document being
        // fetched and committed, which on a cold browser follows the process
        // start it has not finished paying for yet.
        browser.wait_for(id, STARTUP_DEADLINE)?;
        let paused = self.collect_paused(browser);
        self.resolve_fetches(browser, paused)?;

        // Request interception is enabled *after* the document is on its way.
        // Enabled before, it pauses the navigation's own request, and a
        // paused request that nothing resumes is a page that never loads and
        // therefore never produces a frame. Every subsequent request — which
        // is every request a bundle can actually make — is still intercepted
        // and refused unless it stays on the private origin.
        self.command_within(
            browser,
            "Fetch.enable",
            serde_json::json!({ "patterns": [{ "urlPattern": "*" }] }),
            STARTUP_DEADLINE,
        )?;
        Ok(())
    }

    /// Ask the browser for frames at the overlay's own size.
    ///
    /// `maxWidth`/`maxHeight` are what make Chrome encode the frame pulpit
    /// actually wants. Left off, it sends the page's full device-pixel surface
    /// and every frame is then resampled on the worker's own thread — the cost
    /// paid per frame, for a picture the browser could have produced correctly
    /// in the first place.
    ///
    /// `deadline` because this is on both paths: once while the session is
    /// still coming up, and again whenever the cast has to be restarted under
    /// a browser that is long since warm.
    fn start_screencast(
        &mut self,
        browser: &mut Browser,
        deadline: Duration,
    ) -> Result<(), MediaError> {
        self.cast_size = (self.spec.viewport.width, self.spec.viewport.height);
        let (width, height) = self.cast_size;
        // A coarse browser-side throttle: on a 60 Hz compositor every second
        // frame is 30 frames/s. The publish deadline below it is the actual
        // cap; this only saves the browser encoding frames that deadline
        // would discard anyway.
        let every_nth = match self.spec.max_fps {
            0 => 1,
            fps if fps <= 30 => 2,
            _ => 1,
        };
        self.command_within(
            browser,
            "Page.startScreencast",
            serde_json::json!({
                "format": "jpeg",
                "quality": 80,
                "maxWidth": width,
                "maxHeight": height,
                "everyNthFrame": every_nth,
            }),
            deadline,
        )?;
        self.cast_running = true;
        self.next_publish = None;
        Ok(())
    }

    /// Stop the screencast without touching the page: playback continues,
    /// frames stop. The sequence counter is not reset, so the frames of a
    /// later restart still supersede everything published before it.
    fn stop_screencast(&mut self, browser: &mut Browser) -> Result<(), MediaError> {
        if !self.cast_running {
            return Ok(());
        }
        self.cast_running = false;
        self.command(browser, "Page.stopScreencast", serde_json::json!({}))?;
        Ok(())
    }

    /// Follow the presenter on or off this overlay's page.
    ///
    /// Deactivation stops the screencast rather than merely skipping decode:
    /// a page nobody sees must cost neither browser-side JPEG encoding nor
    /// CDP transport. The page itself plays on, so reactivation is only a
    /// restarted cast, not a reloaded document.
    fn set_active(&mut self, browser: &mut Browser, active: bool) -> Result<(), MediaError> {
        if self.active == active {
            return Ok(());
        }
        self.active = active;
        if active {
            if !self.cast_running {
                self.start_screencast(browser, COMMAND_DEADLINE)?;
            }
        } else {
            self.stop_screencast(browser)?;
        }
        Ok(())
    }

    /// Requests this page has had paused since it last looked.
    fn collect_paused(&mut self, browser: &mut Browser) -> Vec<(String, String)> {
        let mut paused = Vec::new();
        for event in browser.take(&self.page.session) {
            collect_fetch_pause(&event, &mut paused);
        }
        paused
    }

    /// Allow requests to the private origin, block everything else.
    fn resolve_fetches(
        &mut self,
        browser: &mut Browser,
        paused: Vec<(String, String)>,
    ) -> Result<(), MediaError> {
        let origin = self._server.origin();
        for (request_id, url) in paused {
            if url.starts_with(&origin) {
                self.send_page(
                    browser,
                    "Fetch.continueRequest",
                    serde_json::json!({ "requestId": request_id }),
                )?;
            } else {
                tracing::info!(%url, "denied a request leaving the bundle's private origin");
                self.send_page(
                    browser,
                    "Fetch.failRequest",
                    serde_json::json!({ "requestId": request_id, "errorReason": "AccessDenied" }),
                )?;
            }
        }
        Ok(())
    }

    /// Handle everything the browser filed for this page, publishing any
    /// screencast frame that arrived.
    fn pump(
        &mut self,
        browser: &mut Browser,
        out: &mut impl Write,
        counters: &mut WorkerCounters,
    ) -> Result<(), MediaError> {
        let mut paused: Vec<(String, String)> = Vec::new();
        let mut frames: Vec<(String, i64)> = Vec::new();

        for mut event in browser.take(&self.page.session) {
            collect_fetch_pause(&event, &mut paused);
            match event.get("method").and_then(Json::as_str) {
                Some("Page.screencastFrame") => {
                    if let Some(params) = event.get_mut("params") {
                        let ack = params
                            .get("sessionId")
                            .and_then(Json::as_i64)
                            .unwrap_or_default();
                        // The base64 payload is moved out of the event rather
                        // than copied: at 60 frames/s of 1080p JPEG the copy
                        // alone is real bandwidth.
                        let data = match params.get_mut("data").map(Json::take) {
                            Some(Json::String(data)) => data,
                            _ => String::new(),
                        };
                        counters.cdp_frames_received += 1;
                        frames.push((data, ack));
                    }
                }
                // The page saying where it has got to. Reported even while the
                // session is inactive: a video that ran on while the presenter
                // was on another slide has moved, and a transport showing the
                // position it had on the way out is showing a lie.
                Some("Runtime.bindingCalled") => {
                    if let Some(progress) = decode_progress(&event) {
                        write_message(
                            out,
                            &MediaEvent::Progress {
                                session: self.spec.session,
                                progress,
                            },
                        )
                        .map_err(|e| MediaError::new(MediaErrorKind::Crashed, e.to_string()))?;
                    }
                }
                _ => {}
            }
        }

        self.resolve_fetches(browser, paused)?;

        // Only the newest frame is worth decoding: the older ones are already
        // stale by the time they would reach the surface. The rest are still
        // acknowledged, or Chrome stops sending: it will not produce a frame
        // while one is outstanding. Frames of a cast that has been stopped
        // are not acknowledged at all — a restart resets the browser's
        // outstanding-frame bookkeeping, so nothing waits on them.
        let newest = frames.pop();
        counters.frames_discarded_before_decode += frames.len() as u64;
        if self.cast_running {
            for (_, ack) in &frames {
                self.send_page(
                    browser,
                    "Page.screencastFrameAck",
                    serde_json::json!({ "sessionId": ack }),
                )?;
            }
        }
        if let Some((data, ack)) = newest {
            if self.cast_running {
                self.send_page(
                    browser,
                    "Page.screencastFrameAck",
                    serde_json::json!({ "sessionId": ack }),
                )?;
            }
            // The publish deadline is the authoritative rate cap:
            // `everyNthFrame` counts compositor frames, so a faster display
            // would defeat it. A frame ahead of the deadline is dropped here,
            // before its decode, scale and copies are paid.
            //
            // The tolerance matters: frames arrive on the compositor's clock
            // and the deadline advances by the same period, so an exact
            // comparison aliases — a frame landing a few milliseconds early
            // every time would halve the rate rather than cap it. Anything
            // within the last two-fifths of an interval counts as on time,
            // and the next deadline advances from the previous one, not from
            // the publish, so the cadence does not drift.
            let due = match (self.next_publish, self.publish_interval) {
                (None, _) => true,
                (Some(at), interval) => {
                    let tolerance =
                        interval.map_or(Duration::ZERO, |interval| interval.mul_f32(0.4));
                    Instant::now() + tolerance >= at
                }
            };
            if self.active && due {
                self.publish(&data, out, counters)?;
                if let Some(interval) = self.publish_interval {
                    let now = Instant::now();
                    self.next_publish = Some(match self.next_publish {
                        // On schedule: stay on cadence. Far behind: resync.
                        Some(at) if at + interval > now => at + interval,
                        _ => now + interval,
                    });
                }
            } else {
                counters.frames_discarded_before_decode += 1;
            }
        }
        Ok(())
    }

    fn publish(
        &mut self,
        base64_jpeg: &str,
        out: &mut impl Write,
        counters: &mut WorkerCounters,
    ) -> Result<(), MediaError> {
        decode_base64_into(base64_jpeg, &mut self.jpeg).ok_or_else(|| {
            MediaError::new(
                MediaErrorKind::ProtocolViolation,
                "the browser sent an unreadable screencast frame",
            )
        })?;
        let decoded = image::load_from_memory(&self.jpeg).map_err(|e| {
            MediaError::new(
                MediaErrorKind::DecodeFailed,
                format!("a screencast frame could not be decoded: {e}"),
            )
        })?;
        counters.frames_decoded += 1;
        // JPEG decodes to RGB; the alpha channel is expanded into a scratch
        // buffer this session keeps, not a fresh allocation per frame.
        let (width, height) = (decoded.width(), decoded.height());
        let raw: &[u8] = match &decoded {
            image::DynamicImage::ImageRgba8(image) => image.as_raw(),
            image::DynamicImage::ImageRgb8(image) => {
                // Sized once, then written through disjoint chunks: growing
                // through `extend_from_slice` paid a capacity check per
                // pixel, which showed up as whole percentage points of
                // worker CPU at 1080p.
                let pixels = image.as_raw().len() / 3;
                self.rgba_scratch.resize(pixels * 4, 0xFF);
                for (source, target) in image
                    .as_raw()
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(self.rgba_scratch.as_chunks_mut::<4>().0)
                {
                    target[..3].copy_from_slice(source);
                    target[3] = 0xFF;
                }
                &self.rgba_scratch
            }
            other => {
                self.rgba_scratch = other.to_rgba8().into_raw();
                &self.rgba_scratch
            }
        };
        let viewport = self.spec.viewport;

        self.sequence += 1;
        // Asked for at the overlay's size, a frame normally arrives at it and
        // is written straight to the ring. The resample stays because Chrome
        // preserves the page's aspect ratio inside the box it was given, so a
        // viewport whose shape differs from the page's still needs fitting —
        // fitting, not stretching: a mismatch shows bars, never a distorted
        // picture.
        let written = if (width, height) == (viewport.width, viewport.height) {
            counters.frames_scale_elided += 1;
            self.ring.write_frame(raw, self.sequence)
        } else {
            counters.frames_scaled += 1;
            let scaled = fit_rgba(raw, width, height, viewport.width, viewport.height);
            self.ring.write_frame(&scaled, self.sequence)
        };
        let slot = written
            .map_err(|e| MediaError::new(MediaErrorKind::ProtocolViolation, e.to_string()))?;
        let Some(slot) = slot else {
            self.sequence -= 1;
            return Ok(());
        };
        counters.frames_published += 1;
        let bytes = u64::from(viewport.width) * u64::from(viewport.height) * 4;
        write_message(
            out,
            &MediaEvent::FrameReady(SurfaceFrame {
                session: self.spec.session,
                surface: self.spec.surface,
                sequence: self.sequence,
                presentation_time: None,
                width: viewport.width,
                height: viewport.height,
                stride: viewport.width * 4,
                format: PixelFormat::Rgba8Straight,
                damage: Vec::new(),
                slot,
                bytes,
            }),
        )
        .map_err(|e| MediaError::new(MediaErrorKind::Crashed, e.to_string()))
    }

    /// Translate one presenter input event into CDP.
    fn input(&mut self, browser: &mut Browser, event: InputEvent) -> Result<(), MediaError> {
        let button_name = |button: PointerButton| match button {
            PointerButton::Left => "left",
            PointerButton::Middle => "middle",
            PointerButton::Right => "right",
        };
        match event {
            InputEvent::PointerMoved { x, y } => {
                self.pointer = (x, y);
                self.send_page(
                    browser,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({ "type": "mouseMoved", "x": x, "y": y }),
                )?;
            }
            InputEvent::PointerPressed {
                x,
                y,
                button,
                click_count,
            } => {
                self.pointer = (x, y);
                self.send_page(
                    browser,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mousePressed", "x": x, "y": y,
                        "button": button_name(button), "clickCount": click_count.max(1),
                        "buttons": 1,
                    }),
                )?;
            }
            InputEvent::PointerReleased {
                x,
                y,
                button,
                click_count,
            } => {
                self.send_page(
                    browser,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mouseReleased", "x": x, "y": y,
                        "button": button_name(button), "clickCount": click_count.max(1),
                        "buttons": 0,
                    }),
                )?;
            }
            InputEvent::Wheel {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                self.send_page(
                    browser,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mouseWheel", "x": x, "y": y,
                        "deltaX": delta_x, "deltaY": delta_y,
                    }),
                )?;
            }
            InputEvent::PointerLeft => {
                // Park the pointer outside the viewport so hover states clear.
                let (width, height) = self.spec.viewport.css_size();
                self.send_page(
                    browser,
                    "Input.dispatchMouseEvent",
                    serde_json::json!({
                        "type": "mouseMoved",
                        "x": width as f32 + 10.0,
                        "y": height as f32 + 10.0,
                    }),
                )?;
            }
            InputEvent::KeyPressed { key, text } => {
                self.send_page(
                    browser,
                    "Input.dispatchKeyEvent",
                    serde_json::json!({
                        "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
                        "key": key,
                        "text": text.unwrap_or_default(),
                    }),
                )?;
            }
            InputEvent::KeyReleased { key } => {
                self.send_page(
                    browser,
                    "Input.dispatchKeyEvent",
                    serde_json::json!({ "type": "keyUp", "key": key }),
                )?;
            }
        }
        Ok(())
    }

    fn set_viewport(
        &mut self,
        browser: &mut Browser,
        viewport: Viewport,
    ) -> Result<(), MediaError> {
        if viewport.validate().is_err() {
            return Ok(());
        }
        let bytes = (viewport.width as u64) * (viewport.height as u64) * 4;
        if bytes > self.spec.slot_bytes {
            return Ok(());
        }
        self.spec.viewport = viewport;
        let (css_width, css_height) = viewport.css_size();
        self.send_page(
            browser,
            "Emulation.setDeviceMetricsOverride",
            device_metrics(css_width, css_height, viewport.scale),
        )?;
        // The screencast carries its frame size from when it started, so a
        // resized overlay would keep arriving at the old size and be resampled
        // to the new one — exactly the per-frame scaling the size was given to
        // avoid. Restarted, Chrome encodes the new size itself. An inactive
        // session has no cast to restart; activation starts one at the size
        // recorded here.
        if self.cast_size != (viewport.width, viewport.height) {
            self.stop_screencast(browser)?;
            self.cast_size = (viewport.width, viewport.height);
            if self.active {
                self.start_screencast(browser, COMMAND_DEADLINE)?;
            }
        }
        Ok(())
    }
}

/// One video command as a call on the page's control object.
///
/// Numbers are formatted through `serde_json`, so a non-finite value becomes
/// `null` rather than the bare `NaN` that would be a syntax error in the
/// expression this builds.
fn video_call(command: crate::protocol::VideoCommand) -> String {
    use crate::protocol::VideoCommand;
    fn number(value: f32) -> Json {
        serde_json::Number::from_f64(value as f64)
            .map(Json::Number)
            .unwrap_or(Json::Null)
    }
    match command {
        VideoCommand::Play => "play()".to_string(),
        VideoCommand::Pause => "pause()".to_string(),
        VideoCommand::Seek { seconds } => format!("seek({})", number(seconds.max(0.0))),
        VideoCommand::SetVolume { level } => format!("volume({})", number(level.clamp(0.0, 1.0))),
        VideoCommand::SetMuted { muted } => format!("mute({muted})"),
        VideoCommand::SetLooping { looping } => format!("loop({looping})"),
    }
}

/// One animated-image command, in the same terms.
fn image_call(command: crate::protocol::ImageCommand) -> String {
    use crate::protocol::ImageCommand;
    match command {
        // An animation has no playhead, so starting it and restarting it are
        // the same act: the page reloads the file from its first frame.
        ImageCommand::Play | ImageCommand::Restart => "play()".to_string(),
        ImageCommand::Pause => "pause()".to_string(),
    }
}

/// Read a `Runtime.bindingCalled` event from the progress binding.
///
/// Anything else — another binding, a malformed payload, a field of the wrong
/// type — is `None`: the page's report is untrusted input like any other, and
/// a transport is better with a stale position than with a fabricated one.
fn decode_progress(event: &Json) -> Option<crate::protocol::PlaybackProgress> {
    let params = event.get("params")?;
    if params.get("name").and_then(Json::as_str)? != PROGRESS_BINDING {
        return None;
    }
    let payload = params.get("payload").and_then(Json::as_str)?;
    let reported: Json = serde_json::from_str(payload).ok()?;
    let number = |key: &str| reported.get(key).and_then(Json::as_f64).map(|v| v as f32);
    let flag = |key: &str| reported.get(key).and_then(Json::as_bool);
    Some(
        crate::protocol::PlaybackProgress {
            position: number("position")?,
            duration: number("duration"),
            paused: flag("paused")?,
            muted: flag("muted").unwrap_or(false),
            volume: number("volume").unwrap_or(1.0),
        }
        .sanitised(),
    )
}

fn collect_fetch_pause(event: &Json, into: &mut Vec<(String, String)>) {
    if event.get("method").and_then(Json::as_str) != Some("Fetch.requestPaused") {
        return;
    }
    let Some(params) = event.get("params") else {
        return;
    };
    let request_id = params
        .get("requestId")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    let url = params
        .get("request")
        .and_then(|request| request.get("url"))
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    if !request_id.is_empty() {
        into.push((request_id, url));
    }
}

/// Every regular file beneath `root`, canonicalised. This is the allowlist
/// the private origin will serve, so a symlink planted in the bundle cannot
/// widen it.
fn collect_files(root: &std::path::Path) -> Result<Vec<PathBuf>, MediaError> {
    const MAX_FILES: usize = 4096;
    let root = root.canonicalize().map_err(|e| {
        MediaError::new(
            MediaErrorKind::LoadFailed,
            format!("the staged bundle is unreadable: {e}"),
        )
    })?;
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                // A canonicalised path that leaves the root is a planted
                // symlink; it is dropped rather than served.
                if let Ok(resolved) = path.canonicalize() {
                    if resolved.starts_with(&root) {
                        files.push(resolved);
                    }
                }
            }
            if files.len() > MAX_FILES {
                return Err(MediaError::new(
                    MediaErrorKind::ResourceLimit,
                    "the bundle contains more files than pulpit will serve",
                ));
            }
        }
    }
    Ok(files)
}

/// Decode standard base64 into a caller-owned buffer, so the per-frame
/// decode reuses one allocation. Written out rather than taking a dependency
/// for the one place a screencast frame needs it.
fn decode_base64_into(value: &str, out: &mut Vec<u8>) -> Option<()> {
    fn sextet(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
            b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    fn decode_chunk(chunk: &[u8], out: &mut Vec<u8>) -> Option<()> {
        if chunk.len() < 2 {
            return None;
        }
        let mut accumulator = 0u32;
        let mut kept = 0;
        for (index, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                accumulator <<= 6;
                continue;
            }
            accumulator = (accumulator << 6) | sextet(*byte)?;
            kept = index + 1;
        }
        accumulator <<= 6 * (4usize.saturating_sub(chunk.len()));
        let decoded = [
            (accumulator >> 16) as u8,
            (accumulator >> 8) as u8,
            accumulator as u8,
        ];
        out.extend_from_slice(&decoded[..kept.saturating_sub(1).min(3)]);
        Some(())
    }
    out.clear();
    out.reserve(value.len() / 4 * 3);
    let mut chunk = [0u8; 4];
    let mut fill = 0;
    for byte in value.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        chunk[fill] = byte;
        fill += 1;
        if fill == 4 {
            decode_chunk(&chunk, out)?;
            fill = 0;
        }
    }
    if fill > 0 {
        decode_chunk(&chunk[..fill], out)?;
    }
    Some(())
}

/// Allocating form of [`decode_base64_into`], for callers without a scratch
/// buffer to reuse.
#[cfg(test)]
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    decode_base64_into(value, &mut out)?;
    Some(out)
}

pub fn run() {
    crate::init_worker_logging(RuntimeId::ExternalChromium);

    let configured = std::env::var_os("PULPIT_BROWSER").map(PathBuf::from);
    let executable = discover_chromium(configured.as_deref());

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    // Buffered over the line-buffered lock: bincode payloads contain
    // incidental newline bytes, and a LineWriter flushes on every one of
    // them, producing short writes at arbitrary points. `write_message`
    // flushes each complete message.
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut sessions: std::collections::HashMap<SessionId, Session> =
        std::collections::HashMap::new();
    // Opened on the first session and kept for the worker's lifetime: a
    // presenter moving between slides is exactly the case that must not pay
    // for a browser start, so the process outlives the sessions using it.
    let mut browser: Option<Browser> = None;

    // Pipeline totals for the whole worker, reported at most once a second
    // and only when something changed.
    let mut counters = WorkerCounters::default();
    let mut reported = WorkerCounters::default();
    let mut last_report = Instant::now();
    // Ring drops of sessions that have since closed, so the total never
    // goes backwards.
    let mut retired_ring_dropped = 0u64;
    // Scratch for the sessions each iteration actually pumps, reused so the
    // 250 Hz loop does not allocate.
    let mut pump_ids: Vec<SessionId> = Vec::new();

    loop {
        // Serve the browser first so animation keeps flowing, then look for
        // a command. With no *active* session there are no frames to race
        // for — only progress events and backpressure to keep drained — so
        // the loop slows from 250 Hz to 20 Hz. Reactivation arrives on
        // stdin, which is waited on below with the same relaxed deadline.
        let tick = if sessions.values().any(|session| session.active) {
            IDLE_TICK
        } else {
            PARKED_TICK
        };
        if !sessions.is_empty() {
            if let Some(engine) = browser.as_mut() {
                // One read for every session: the pipe is shared, so a
                // message is filed for the page it belongs to and each
                // session then handles its own. Only sessions with something
                // to do are pumped — an inactive session with an empty inbox
                // has no frames to publish and no events to report, so the
                // idle loop's cost follows the *active* set, not everything
                // ever opened. A failed read is for everyone: each session
                // must be told the browser is gone.
                let read = engine.poll(tick);
                pump_ids.clear();
                pump_ids.extend(sessions.iter().filter_map(|(id, session)| {
                    (read.is_err() || session.active || engine.has_events(&session.page.session))
                        .then_some(*id)
                }));
                for &id in &pump_ids {
                    let Some(session) = sessions.get_mut(&id) else {
                        continue;
                    };
                    // A failed read is the browser itself failing, so every
                    // session on it is told rather than only the next one to
                    // notice.
                    let outcome = match &read {
                        Ok(()) => session.pump(engine, &mut out, &mut counters),
                        Err(error) => Err(error.clone()),
                    };
                    if let Err(error) = outcome {
                        reply::failed(&mut out, Some(id), error);
                        if let Some(session) = sessions.remove(&id) {
                            retired_ring_dropped += session.ring.dropped();
                        }
                    }
                }
                if read.is_err() {
                    // The pipe is unusable; the next Open starts a new one.
                    browser = None;
                }
            }
            // Totals go out on a slow clock, and only when they moved: an
            // idle worker reports nothing.
            if last_report.elapsed() >= Duration::from_secs(1) {
                counters.ring_dropped =
                    retired_ring_dropped + sessions.values().map(|s| s.ring.dropped()).sum::<u64>();
                if counters != reported {
                    let _ = write_message(&mut out, &MediaEvent::Counters(counters));
                    reported = counters;
                }
                last_report = Instant::now();
            }

            // `wait_for_input`, not a bare poll of the descriptor. One read
            // fills the buffered reader with everything that had arrived, so
            // a burst — the set-active, viewport and input messages a slide
            // change sends together — leaves later messages sitting in the
            // buffer while the descriptor itself looks empty. Polling alone
            // then waits for ever on commands that have already arrived,
            // which is why an overlay stopped responding after the presenter
            // moved off its slide and came back.
            if !crate::worker::wait_for_input(&mut reader, tick) {
                continue;
            }
        }

        let request: MediaRequest = match read_message(&mut reader) {
            Ok(request) => request,
            Err(_) => break,
        };

        match request {
            MediaRequest::Hello { version } => {
                reply::hello(
                    &mut out,
                    version,
                    RuntimeId::ExternalChromium,
                    vec!["chromium-runtime".to_string()],
                );
            }
            MediaRequest::Probe(_) => {
                let _ = write_message(
                    &mut out,
                    &MediaEvent::ProbeResult(Box::new(probe_external_chromium(
                        configured.as_deref(),
                    ))),
                );
            }
            MediaRequest::Open(spec) => {
                let id = spec.session;
                let viewport = spec.viewport;
                let result = match executable.as_deref() {
                    Some(executable) => {
                        // Launched here rather than at start-up: a worker that
                        // is only ever probed must not leave a browser running.
                        if browser.is_none() {
                            match Browser::launch(executable, viewport) {
                                Ok(engine) => browser = Some(engine),
                                Err(error) => {
                                    reply::failed(&mut out, Some(id), error);
                                    continue;
                                }
                            }
                        }
                        Session::open(*spec, browser.as_mut().expect("just launched"))
                    }
                    None => Err(MediaError::new(
                        MediaErrorKind::Unavailable,
                        "no Chromium-family browser is installed",
                    )),
                };
                match result {
                    Ok(session) => {
                        reply::ready(&mut out, id);
                        let _ = write_message(
                            &mut out,
                            &MediaEvent::StateChanged {
                                session: id,
                                state: SessionState::Playing,
                            },
                        );
                        sessions.insert(id, session);
                    }
                    Err(error) => {
                        reply::failed(&mut out, Some(id), error);
                    }
                }
            }
            MediaRequest::Close { session } => {
                if let Some(state) = sessions.remove(&session) {
                    retired_ring_dropped += state.ring.dropped();
                    // The page goes back to the browser's pool rather than
                    // taking the process down with it.
                    if let Some(engine) = browser.as_mut() {
                        engine.release_page(state.page.clone());
                    }
                }
                reply::closed(&mut out, session);
            }
            MediaRequest::SetActive { session, active } => {
                if let (Some(state), Some(engine)) = (sessions.get_mut(&session), browser.as_mut())
                {
                    if let Err(error) = state.set_active(engine, active) {
                        reply::failed(&mut out, Some(session), error);
                    }
                }
            }
            MediaRequest::SetViewport { session, viewport } => {
                if let (Some(state), Some(engine)) = (sessions.get_mut(&session), browser.as_mut())
                {
                    let _ = state.set_viewport(engine, viewport);
                }
            }
            MediaRequest::SetFocus { .. } => {}
            MediaRequest::Input { session, event } => {
                if event.validate().is_err() {
                    continue;
                }
                if let (Some(state), Some(engine)) = (sessions.get_mut(&session), browser.as_mut())
                {
                    if let Err(error) = state.input(engine, event) {
                        reply::failed(&mut out, Some(session), error);
                    }
                }
            }
            MediaRequest::Web { session, command } => {
                if let (Some(state), Some(engine)) = (sessions.get_mut(&session), browser.as_mut())
                {
                    match command {
                        WebCommand::Reload => {
                            let _ = state.send_page(
                                engine,
                                "Page.reload",
                                serde_json::json!({ "ignoreCache": true }),
                            );
                        }
                        // There is deliberately no host-side `eval`: a posted
                        // message is delivered as data to a page listener.
                        WebCommand::Post { value } => {
                            let truncated: String = value
                                .chars()
                                .take(crate::protocol::MAX_WEB_MESSAGE_BYTES)
                                .collect();
                            let _ = state.send_page(
                                engine,
                                "Runtime.evaluate",
                                serde_json::json!({
                                    "expression": format!(
                                        "window.dispatchEvent(new MessageEvent('pulpit', {{ data: {} }}))",
                                        serde_json::Value::String(truncated)
                                    ),
                                    "returnByValue": true,
                                }),
                            );
                        }
                    }
                }
            }
            MediaRequest::ReleaseFrame {
                session,
                slot,
                sequence,
            } => {
                if let Some(state) = sessions.get_mut(&session) {
                    state.ring.release(slot, sequence);
                }
            }
            // Both content kinds are played by the same document, so both are
            // driven through the one control object it exposes. The call is
            // built here rather than passed through, so nothing the
            // application sends is ever evaluated as script.
            MediaRequest::Image { session, command } => {
                if let (Some(state), Some(engine)) = (sessions.get_mut(&session), browser.as_mut())
                {
                    let _ = state.call_control(engine, &image_call(command));
                }
            }
            MediaRequest::Video { session, command } => {
                if let (Some(state), Some(engine)) = (sessions.get_mut(&session), browser.as_mut())
                {
                    let _ = state.call_control(engine, &video_call(command));
                }
            }
            MediaRequest::Shutdown => break,
        }
    }

    // Dropping the browser closes it and removes its private profile.
    sessions.clear();
    drop(browser);
    tracing::debug!("chromium worker exiting");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_the_shapes_a_screencast_frame_arrives_in() {
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        assert_eq!(decode_base64("TQ==").unwrap(), b"M");
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        // JPEG magic, as a screencast frame would begin.
        assert_eq!(decode_base64("/9j/").unwrap(), vec![0xFF, 0xD8, 0xFF]);
    }

    #[test]
    fn base64_with_a_bad_character_is_refused_rather_than_guessed_at() {
        assert!(decode_base64("TW!u").is_none());
    }

    #[test]
    fn a_fetch_pause_event_yields_its_request_and_url() {
        let mut collected = Vec::new();
        collect_fetch_pause(
            &serde_json::json!({
                "method": "Fetch.requestPaused",
                "params": {
                    "requestId": "req-1",
                    "request": { "url": "https://example.com/tracker.js" },
                },
            }),
            &mut collected,
        );
        assert_eq!(
            collected,
            vec![(
                "req-1".to_string(),
                "https://example.com/tracker.js".to_string()
            )]
        );
    }

    #[test]
    fn other_browser_events_are_not_mistaken_for_fetch_pauses() {
        let mut collected = Vec::new();
        collect_fetch_pause(
            &serde_json::json!({ "method": "Page.screencastFrame", "params": {} }),
            &mut collected,
        );
        assert!(collected.is_empty());
    }

    #[test]
    fn the_allowlist_covers_the_staged_bundle_and_stops_at_its_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("bundle");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(root.join("assets/app.js"), b"//").unwrap();
        std::fs::write(directory.path().join("outside.txt"), b"secret").unwrap();

        let files = collect_files(&root).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .all(|file| file.starts_with(root.canonicalize().unwrap())));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_out_of_the_bundle_is_not_allowlisted() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("bundle");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), b"<h1>hi</h1>").unwrap();
        let secret = directory.path().join("secret.txt");
        std::fs::write(&secret, b"not for the page").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("escape.txt")).unwrap();

        let files = collect_files(&root).unwrap();
        assert_eq!(
            files.len(),
            1,
            "the planted symlink resolves outside the root and is dropped"
        );
    }
}
