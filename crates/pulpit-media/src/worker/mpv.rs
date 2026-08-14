//! The libmpv media worker: plain video and animated images, with audio.
//!
//! Ships no decoder. Discovers an installed (or bundled-beside-the-binary)
//! `libmpv`, drives it through its C API loaded at runtime, and renders
//! frames through mpv's software render API straight into the shared-memory
//! ring. No JPEG round trip, no browser process: this is the cheap path for
//! everything that is not HTML, and the Chromium screencast remains the
//! fallback chain below it.
//!
//! stdout is the protocol stream, so nothing else may ever be printed there.

use std::ffi::{c_char, c_int, c_void, CString};
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::protocol::{
    read_message, write_message, ImageCommand, InputEvent, MediaError, MediaErrorKind, MediaEvent,
    MediaRequest, PixelFormat, PlaybackProgress, RuntimeId, SessionId, SessionSource, SessionSpec,
    SurfaceFrame, VideoCommand, WorkerDescription, MEDIA_PROTOCOL_VERSION,
};
use crate::surface::AttachedRing;

/// How often the loop looks for commands and new frames.
const TICK: Duration = Duration::from_millis(8);
/// How often playback position is reported.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(400);

// mpv_render_param types and property formats, from `mpv/render.h` and
// `mpv/client.h`. ABI constants: stable by mpv's own compatibility promise.
const PARAM_API_TYPE: c_int = 1;
const PARAM_SW_SIZE: c_int = 17;
const PARAM_SW_FORMAT: c_int = 18;
const PARAM_SW_STRIDE: c_int = 19;
const PARAM_SW_POINTER: c_int = 20;
const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_DOUBLE: c_int = 5;

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

/// The handful of libmpv symbols this worker needs, resolved at runtime.
pub struct Api {
    _library: libloading::Library,
    create: unsafe extern "C" fn() -> *mut c_void,
    initialize: unsafe extern "C" fn(*mut c_void) -> c_int,
    destroy: unsafe extern "C" fn(*mut c_void),
    set_option: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    set_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    render_create: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int,
    render: unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int,
    render_free: unsafe extern "C" fn(*mut c_void),
}

/// Where a libmpv may live: an explicit override, bundled beside the
/// executable, the platform's usual profile paths, then the loader's own
/// search. Mirrors the PDFium search order.
pub fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PULPIT_LIBMPV") {
        candidates.push(PathBuf::from(path));
    }
    let names: &[&str] = if cfg!(target_os = "macos") {
        &["libmpv.2.dylib", "libmpv.dylib"]
    } else if cfg!(windows) {
        &["mpv-2.dll", "libmpv-2.dll"]
    } else {
        &["libmpv.so.2", "libmpv.so"]
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in names {
                candidates.push(dir.join(name));
                candidates.push(dir.join("lib").join(name));
            }
        }
    }
    // Homebrew is how a Mac gets libmpv, and dyld does not search its prefix:
    // unlike the Linux loader with /usr/lib, nothing puts /opt/homebrew/lib on
    // the default path, so the bare-name fallback below never finds it.
    // Without these two entries `brew install mpv` appears to do nothing.
    #[cfg(target_os = "macos")]
    {
        for prefix in ["/opt/homebrew/lib", "/usr/local/lib"] {
            for name in names {
                candidates.push(PathBuf::from(prefix).join(name));
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for profile in [
            dirs_home().map(|home| home.join(".nix-profile/lib")),
            std::env::var_os("USER").map(|user| {
                PathBuf::from("/etc/profiles/per-user")
                    .join(user)
                    .join("lib")
            }),
            Some(PathBuf::from("/run/current-system/sw/lib")),
        ]
        .into_iter()
        .flatten()
        {
            for name in names {
                candidates.push(profile.join(name));
            }
        }
    }
    for name in names {
        candidates.push(PathBuf::from(name));
    }
    candidates
}

#[cfg(target_os = "linux")]
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl Api {
    pub fn load() -> Result<Api, MediaError> {
        let mut detail = String::new();
        for candidate in library_candidates() {
            // SAFETY: loading a shared library runs its initialisers; libmpv
            // is the library this worker exists to load.
            match unsafe { libloading::Library::new(&candidate) } {
                Ok(library) => match Api::bind(library) {
                    Ok(api) => {
                        tracing::info!(path = %candidate.display(), "bound to libmpv");
                        return Ok(api);
                    }
                    Err(e) => detail = e,
                },
                Err(e) => detail = e.to_string(),
            }
        }
        Err(MediaError::new(
            MediaErrorKind::Unavailable,
            format!("no usable libmpv was found: {detail}"),
        ))
    }

    fn bind(library: libloading::Library) -> Result<Api, String> {
        /// Resolve one symbol at its declared function type.
        macro_rules! symbol {
            ($name:literal as $ty:ty) => {
                // SAFETY: the name and signature are libmpv's documented,
                // ABI-stable C API.
                unsafe {
                    match library.get::<$ty>($name) {
                        Ok(symbol) => *symbol,
                        Err(e) => return Err(e.to_string()),
                    }
                }
            };
        }
        Ok(Api {
            create: symbol!(b"mpv_create\0" as unsafe extern "C" fn() -> *mut c_void),
            initialize: symbol!(b"mpv_initialize\0" as unsafe extern "C" fn(*mut c_void) -> c_int),
            destroy: symbol!(b"mpv_terminate_destroy\0" as unsafe extern "C" fn(*mut c_void)),
            set_option: symbol!(
                b"mpv_set_option_string\0"
                    as unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int
            ),
            command: symbol!(
                b"mpv_command\0" as unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int
            ),
            get_property: symbol!(
                b"mpv_get_property\0"
                    as unsafe extern "C" fn(
                        *mut c_void,
                        *const c_char,
                        c_int,
                        *mut c_void,
                    ) -> c_int
            ),
            set_property: symbol!(
                b"mpv_set_property\0"
                    as unsafe extern "C" fn(
                        *mut c_void,
                        *const c_char,
                        c_int,
                        *mut c_void,
                    ) -> c_int
            ),
            render_create: symbol!(
                b"mpv_render_context_create\0"
                    as unsafe extern "C" fn(
                        *mut *mut c_void,
                        *mut c_void,
                        *mut RenderParam,
                    ) -> c_int
            ),
            render: symbol!(
                b"mpv_render_context_render\0"
                    as unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int
            ),
            render_free: symbol!(b"mpv_render_context_free\0" as unsafe extern "C" fn(*mut c_void)),
            _library: library,
        })
    }
}

struct Session {
    spec: SessionSpec,
    ring: AttachedRing,
    handle: *mut c_void,
    render: *mut c_void,
    /// RGBA scratch the software renderer draws into, reused across frames.
    scratch: Vec<u8>,
    sequence: u64,
    active: bool,
    /// Whether playback was paused when the presenter left the slide, so
    /// returning restores their choice rather than always resuming.
    paused_before_parking: bool,
    last_position: f64,
    last_progress: Instant,
}

impl Session {
    fn open(api: &Api, spec: SessionSpec) -> Result<Session, MediaError> {
        spec.validate()
            .map_err(|e| MediaError::new(MediaErrorKind::ProtocolViolation, e.to_string()))?;
        let path = match &spec.source {
            SessionSource::File { path } => path.clone(),
            SessionSource::Bundle { .. } => {
                return Err(MediaError::new(
                    MediaErrorKind::Incompatible,
                    "libmpv plays files, not web bundles",
                ))
            }
        };
        let ring = AttachedRing::attach(&spec.ring_name, spec.slots, spec.slot_bytes)
            .map_err(|e| MediaError::new(MediaErrorKind::ResourceLimit, e.to_string()))?;

        // SAFETY: the documented client-API lifecycle — create, options,
        // initialize, render-context, loadfile.
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err(MediaError::new(
                MediaErrorKind::LaunchFailed,
                "libmpv would not create a player",
            ));
        }
        let option = |name: &str, value: &str| {
            let name = CString::new(name).expect("option name");
            let value = CString::new(value).expect("option value");
            unsafe { (api.set_option)(handle, name.as_ptr(), value.as_ptr()) };
        };
        option("vo", "libmpv");
        option("hwdec", "auto-safe");
        // Hold the final frame at end of file instead of going black.
        option("keep-open", "yes");
        option("audio-display", "no");
        // Local files only: the deck is untrusted input.
        option("ytdl", "no");
        option("load-scripts", "no");
        option("osc", "no");
        option("input-default-bindings", "no");
        if spec.playback.repeat {
            option("loop-file", "inf");
        }
        if spec.playback.mute {
            option("mute", "yes");
        }
        if !spec.playback.autoplay {
            option("pause", "yes");
        }
        if spec.playback.start > 0.0 {
            option("start", &format!("{}", spec.playback.start.max(0.0)));
        }
        if unsafe { (api.initialize)(handle) } < 0 {
            unsafe { (api.destroy)(handle) };
            return Err(MediaError::new(
                MediaErrorKind::LaunchFailed,
                "libmpv would not initialise",
            ));
        }

        let api_type = CString::new("sw").expect("api type");
        let mut params = [
            RenderParam {
                kind: PARAM_API_TYPE,
                data: api_type.as_ptr() as *mut c_void,
            },
            RenderParam {
                kind: 0,
                data: std::ptr::null_mut(),
            },
        ];
        let mut render: *mut c_void = std::ptr::null_mut();
        if unsafe { (api.render_create)(&mut render, handle, params.as_mut_ptr()) } < 0 {
            unsafe { (api.destroy)(handle) };
            return Err(MediaError::new(
                MediaErrorKind::LaunchFailed,
                "libmpv has no software renderer",
            ));
        }

        let file = CString::new(path.as_str())
            .map_err(|_| MediaError::new(MediaErrorKind::MalformedAsset, "path contains NUL"))?;
        let loadfile = CString::new("loadfile").expect("command");
        let mut argv = [loadfile.as_ptr(), file.as_ptr(), std::ptr::null::<c_char>()];
        if unsafe { (api.command)(handle, argv.as_mut_ptr()) } < 0 {
            unsafe {
                (api.render_free)(render);
                (api.destroy)(handle);
            }
            return Err(MediaError::new(
                MediaErrorKind::LoadFailed,
                "libmpv refused the file",
            ));
        }

        Ok(Session {
            scratch: vec![0u8; spec.viewport.width as usize * spec.viewport.height as usize * 4],
            spec,
            ring,
            handle,
            render,
            sequence: 0,
            active: true,
            paused_before_parking: false,
            last_position: -1.0,
            last_progress: Instant::now(),
        })
    }

    fn set_flag(&self, api: &Api, name: &str, value: bool) {
        let name = CString::new(name).expect("property name");
        let mut flag: c_int = value.into();
        unsafe {
            (api.set_property)(
                self.handle,
                name.as_ptr(),
                FORMAT_FLAG,
                &mut flag as *mut c_int as *mut c_void,
            )
        };
    }

    fn set_double(&self, api: &Api, name: &str, value: f64) {
        let name = CString::new(name).expect("property name");
        let mut value = value;
        unsafe {
            (api.set_property)(
                self.handle,
                name.as_ptr(),
                FORMAT_DOUBLE,
                &mut value as *mut f64 as *mut c_void,
            )
        };
    }

    fn get_double(&self, api: &Api, name: &str) -> Option<f64> {
        let name = CString::new(name).expect("property name");
        let mut value: f64 = 0.0;
        let ok = unsafe {
            (api.get_property)(
                self.handle,
                name.as_ptr(),
                FORMAT_DOUBLE,
                &mut value as *mut f64 as *mut c_void,
            )
        };
        (ok >= 0).then_some(value)
    }

    fn get_flag(&self, api: &Api, name: &str) -> bool {
        let name = CString::new(name).expect("property name");
        let mut value: c_int = 0;
        unsafe {
            (api.get_property)(
                self.handle,
                name.as_ptr(),
                FORMAT_FLAG,
                &mut value as *mut c_int as *mut c_void,
            )
        };
        value != 0
    }

    fn seek(&self, api: &Api, seconds: f32) {
        let command = CString::new("seek").expect("command");
        let position = CString::new(format!("{}", seconds.max(0.0))).expect("seconds");
        let mode = CString::new("absolute").expect("mode");
        let mut argv = [
            command.as_ptr(),
            position.as_ptr(),
            mode.as_ptr(),
            std::ptr::null::<c_char>(),
        ];
        unsafe { (api.command)(self.handle, argv.as_mut_ptr()) };
    }

    /// Render the current frame into the ring when playback has moved.
    fn pump(&mut self, api: &Api, out: &mut impl Write) -> Result<(), MediaError> {
        let position = self.get_double(api, "time-pos").unwrap_or(-1.0);
        let first = self.sequence == 0;
        if self.active && (first || position != self.last_position) {
            let width = self.spec.viewport.width;
            let height = self.spec.viewport.height;
            let mut size: [c_int; 2] = [width as c_int, height as c_int];
            // "rgb0": 4 bytes per pixel, R,G,B then padding the consumer
            // treats as alpha — forced opaque below.
            let format = CString::new("rgb0").expect("format");
            let mut stride: usize = width as usize * 4;
            let mut params = [
                RenderParam {
                    kind: PARAM_SW_SIZE,
                    data: size.as_mut_ptr() as *mut c_void,
                },
                RenderParam {
                    kind: PARAM_SW_FORMAT,
                    data: format.as_ptr() as *mut c_void,
                },
                RenderParam {
                    kind: PARAM_SW_STRIDE,
                    data: &mut stride as *mut usize as *mut c_void,
                },
                RenderParam {
                    kind: PARAM_SW_POINTER,
                    data: self.scratch.as_mut_ptr() as *mut c_void,
                },
                RenderParam {
                    kind: 0,
                    data: std::ptr::null_mut(),
                },
            ];
            let drawn = unsafe { (api.render)(self.render, params.as_mut_ptr()) };
            if drawn >= 0 {
                for pixel in self.scratch.chunks_exact_mut(4) {
                    pixel[3] = 0xFF;
                }
                self.sequence += 1;
                match self.ring.write_frame(&self.scratch, self.sequence) {
                    Ok(Some(slot)) => {
                        write_message(
                            out,
                            &MediaEvent::FrameReady(SurfaceFrame {
                                session: self.spec.session,
                                surface: self.spec.surface,
                                sequence: self.sequence,
                                presentation_time: None,
                                width,
                                height,
                                stride: width * 4,
                                format: PixelFormat::Rgba8Straight,
                                damage: Vec::new(),
                                slot,
                                bytes: self.scratch.len() as u64,
                            }),
                        )
                        .map_err(|e| MediaError::new(MediaErrorKind::Crashed, e.to_string()))?;
                        self.last_position = position;
                    }
                    Ok(None) => self.sequence -= 1,
                    Err(e) => {
                        return Err(MediaError::new(
                            MediaErrorKind::ProtocolViolation,
                            e.to_string(),
                        ))
                    }
                }
            }
        }

        if self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            self.last_progress = Instant::now();
            if position >= 0.0 {
                let progress = PlaybackProgress {
                    position: position as f32,
                    duration: self.get_double(api, "duration").map(|d| d as f32),
                    paused: self.get_flag(api, "pause"),
                    muted: self.get_flag(api, "mute"),
                    volume: (self.get_double(api, "volume").unwrap_or(100.0) / 100.0) as f32,
                }
                .sanitised();
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
        Ok(())
    }

    /// Leaving the slide pauses playback — with audio, a video running on a
    /// slide nobody is looking at would keep talking over the presenter.
    fn set_active(&mut self, api: &Api, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            if !self.paused_before_parking {
                self.set_flag(api, "pause", false);
            }
            // Force a fresh frame even if the position is unchanged.
            self.last_position = -1.0;
        } else {
            self.paused_before_parking = self.get_flag(api, "pause");
            self.set_flag(api, "pause", true);
        }
    }

    fn video(&mut self, api: &Api, command: VideoCommand) {
        match command {
            VideoCommand::Play => self.set_flag(api, "pause", false),
            VideoCommand::Pause => self.set_flag(api, "pause", true),
            VideoCommand::Seek { seconds } => {
                self.seek(api, seconds);
                self.last_position = -1.0;
            }
            VideoCommand::SetVolume { level } => {
                self.set_double(api, "volume", (level.clamp(0.0, 1.0) * 100.0) as f64)
            }
            VideoCommand::SetMuted { muted } => self.set_flag(api, "mute", muted),
            VideoCommand::SetLooping { looping } => {
                let value = if looping { "inf" } else { "no" };
                let name = CString::new("loop-file").expect("property");
                let value = CString::new(value).expect("value");
                unsafe {
                    (api.set_property)(
                        self.handle,
                        name.as_ptr(),
                        FORMAT_STRING,
                        &mut (value.as_ptr()) as *mut *const c_char as *mut c_void,
                    )
                };
            }
        }
    }

    fn image(&mut self, api: &Api, command: ImageCommand) {
        match command {
            ImageCommand::Play => self.set_flag(api, "pause", false),
            ImageCommand::Pause => self.set_flag(api, "pause", true),
            ImageCommand::Restart => {
                self.seek(api, 0.0);
                self.set_flag(api, "pause", false);
                self.last_position = -1.0;
            }
        }
    }
}

pub fn run() {
    crate::init_worker_logging(RuntimeId::LibMpv);
    let api = match Api::load() {
        Ok(api) => api,
        Err(error) => {
            // Report through the protocol on the first Open; loading is
            // retried then so the message reaches a session the supervisor
            // can fall back from.
            tracing::warn!(%error, "libmpv unavailable at startup");
            run_without_library();
            return;
        }
    };

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut sessions: std::collections::HashMap<SessionId, Session> =
        std::collections::HashMap::new();

    loop {
        if !sessions.is_empty() {
            let mut failed: Vec<SessionId> = Vec::new();
            for (id, session) in sessions.iter_mut() {
                if let Err(error) = session.pump(&api, &mut out) {
                    let _ = write_message(
                        &mut out,
                        &MediaEvent::Failed {
                            session: Some(*id),
                            error,
                        },
                    );
                    failed.push(*id);
                }
            }
            for id in failed {
                if let Some(session) = sessions.remove(&id) {
                    unsafe {
                        (api.render_free)(session.render);
                        (api.destroy)(session.handle);
                    }
                }
            }
            if !crate::worker::wait_for_input(&mut reader, TICK) {
                continue;
            }
        }

        let request: MediaRequest = match read_message(&mut reader) {
            Ok(request) => request,
            Err(_) => break,
        };
        match request {
            MediaRequest::Hello { version } => {
                if version != MEDIA_PROTOCOL_VERSION {
                    tracing::warn!(theirs = version, ours = MEDIA_PROTOCOL_VERSION, "version");
                }
                let _ = write_message(
                    &mut out,
                    &MediaEvent::Hello(WorkerDescription {
                        version: MEDIA_PROTOCOL_VERSION,
                        runtime: RuntimeId::LibMpv,
                        features: vec!["libmpv".to_string()],
                    }),
                );
            }
            MediaRequest::Probe(_) => {
                let _ = write_message(
                    &mut out,
                    &MediaEvent::ProbeResult(Box::new(crate::runtime::probe_libmpv())),
                );
            }
            MediaRequest::Open(spec) => {
                let id = spec.session;
                match Session::open(&api, *spec) {
                    Ok(session) => {
                        let _ = write_message(&mut out, &MediaEvent::Ready { session: id });
                        sessions.insert(id, session);
                    }
                    Err(error) => {
                        let _ = write_message(
                            &mut out,
                            &MediaEvent::Failed {
                                session: Some(id),
                                error,
                            },
                        );
                    }
                }
            }
            MediaRequest::Close { session } => {
                if let Some(state) = sessions.remove(&session) {
                    unsafe {
                        (api.render_free)(state.render);
                        (api.destroy)(state.handle);
                    }
                }
                let _ = write_message(&mut out, &MediaEvent::Closed { session });
            }
            MediaRequest::SetActive { session, active } => {
                if let Some(state) = sessions.get_mut(&session) {
                    state.set_active(&api, active);
                }
            }
            MediaRequest::Video { session, command } => {
                if let Some(state) = sessions.get_mut(&session) {
                    state.video(&api, command);
                }
            }
            MediaRequest::Image { session, command } => {
                if let Some(state) = sessions.get_mut(&session) {
                    state.image(&api, command);
                }
            }
            MediaRequest::Input { session, event } => {
                // Click parity with the browser wrapper: a press toggles
                // playback, and paints nothing the audience can see.
                if let (Some(state), InputEvent::PointerPressed { .. }) =
                    (sessions.get_mut(&session), &event)
                {
                    let paused = state.get_flag(&api, "pause");
                    state.set_flag(&api, "pause", !paused);
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
            MediaRequest::SetViewport { .. }
            | MediaRequest::SetFocus { .. }
            | MediaRequest::Web { .. } => {}
            MediaRequest::Shutdown => break,
        }
    }

    for (_, session) in sessions.drain() {
        unsafe {
            (api.render_free)(session.render);
            (api.destroy)(session.handle);
        }
    }
    tracing::debug!("mpv worker exiting");
}

/// The protocol loop for a worker whose library never loaded: every Open
/// fails with `Unavailable`, which is exactly what lets the supervisor fall
/// back to the browser.
fn run_without_library() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    loop {
        let request: MediaRequest = match read_message(&mut reader) {
            Ok(request) => request,
            Err(_) => return,
        };
        match request {
            MediaRequest::Hello { .. } => {
                let _ = write_message(
                    &mut out,
                    &MediaEvent::Hello(WorkerDescription {
                        version: MEDIA_PROTOCOL_VERSION,
                        runtime: RuntimeId::LibMpv,
                        features: Vec::new(),
                    }),
                );
            }
            MediaRequest::Open(spec) => {
                let _ = write_message(
                    &mut out,
                    &MediaEvent::Failed {
                        session: Some(spec.session),
                        error: MediaError::new(
                            MediaErrorKind::Unavailable,
                            "no usable libmpv was found",
                        ),
                    },
                );
            }
            MediaRequest::Shutdown => return,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The search has to cover both ways libmpv arrives: bundled beside the
    /// executable, and installed by whatever this platform's users use.
    #[test]
    fn the_search_covers_where_this_platform_puts_libmpv() {
        let candidates = library_candidates();
        assert!(!candidates.is_empty());

        let ends_with = |needle: &str| {
            candidates
                .iter()
                .any(|c| c.to_string_lossy().ends_with(needle))
        };
        let contains = |needle: &str| {
            candidates
                .iter()
                .any(|c| c.to_string_lossy().contains(needle))
        };

        // A bundled copy: the packaging convention is beside the binary, or
        // in a `lib` directory next to it.
        #[cfg(target_os = "macos")]
        {
            assert!(ends_with("libmpv.2.dylib"));
            // dyld does not search Homebrew's prefix, so it must be named.
            assert!(contains("/opt/homebrew/lib"));
            assert!(contains("/usr/local/lib"));
        }
        #[cfg(target_os = "windows")]
        {
            assert!(ends_with("mpv-2.dll"));
        }
        #[cfg(target_os = "linux")]
        {
            assert!(ends_with("libmpv.so.2"));
            // Nix puts nothing on the default loader path.
            assert!(contains("current-system/sw/lib"));
        }

        // The bare name comes last, so the system loader gets the final say
        // rather than the first.
        let last = candidates.last().unwrap().to_string_lossy().to_string();
        assert!(
            !last.contains(std::path::MAIN_SEPARATOR),
            "the last candidate is a bare name for the loader to resolve: {last}"
        );
    }
}
