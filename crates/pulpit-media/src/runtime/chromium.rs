//! The external Chromium-family HTML runtime (`docs-src/internals.typ`).
//!
//! pulpit ships no browser engine. This adapter discovers an *installed*
//! Chromium-family browser, launches it headless with a private profile it
//! created itself, and drives it over the Chrome DevTools Protocol through an
//! inherited pipe rather than a debugging port on the network.
//!
//! The user's own browser profile — extensions, cookies, logins, passwords —
//! is deliberately unreachable, and that is a feature: a slide deck is
//! untrusted code and must never run inside the presenter's browsing session.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::protocol::{MediaError, MediaErrorKind, Viewport};

/// CDP methods this adapter cannot work without. They are feature-probed
/// before any document content is loaded, so a browser that merely *looks*
/// like Chrome fails during startup rather than on stage.
pub const REQUIRED_DOMAINS: &[&str] = &["Page", "Input", "Runtime", "Fetch", "Emulation"];

/// Flags that would defeat the security model. Kept as a list so the refusal
/// is testable: pulpit must never make a deck work by disabling the
/// browser sandbox.
pub const FORBIDDEN_FLAGS: &[&str] = &[
    "--no-sandbox",
    "--disable-web-security",
    "--allow-file-access-from-files",
    "--allow-running-insecure-content",
    "--disable-site-isolation-trials",
];

/// Ask an executable for its version string.
///
/// This is also the first half of the compatibility probe: a program that
/// does not answer `--version` with something Chromium-shaped is not driven
/// any further.
pub fn browser_version(executable: &Path) -> Option<String> {
    let output = Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    is_chromium_version(&text).then_some(text)
}

/// Does this version banner belong to a Chromium-family browser?
pub fn is_chromium_version(banner: &str) -> bool {
    let lowered = banner.to_ascii_lowercase();
    if lowered.contains("firefox") || lowered.contains("safari") {
        return false;
    }
    ["chrome", "chromium", "edge", "brave"]
        .iter()
        .any(|name| lowered.contains(name))
        && banner.chars().any(|character| character.is_ascii_digit())
}

/// The launch flags pulpit uses.
///
/// Every one of these is either required for off-screen rendering or is part
/// of the isolation guarantee. Notably absent: anything from
/// [`FORBIDDEN_FLAGS`].
pub fn launch_flags(profile: &Path, viewport: Viewport) -> Vec<String> {
    let (css_width, css_height) = viewport.css_size();
    let mut user_data_dir = std::ffi::OsString::from("--user-data-dir=");
    user_data_dir.push(profile);
    vec![
        "--headless=new".to_string(),
        "--remote-debugging-pipe".to_string(),
        // Chrome 136 and newer refuse remote debugging on the default
        // profile. pulpit requires a private one for *every* version, so
        // behaviour does not depend on that boundary.
        user_data_dir
            .into_string()
            .unwrap_or_else(|os_str| format!("--user-data-dir={}", os_str.to_string_lossy())),
        format!("--window-size={css_width},{css_height}"),
        format!("--force-device-scale-factor={}", viewport.scale),
        "--hide-scrollbars".to_string(),
        "--mute-audio".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-extensions".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-component-update".to_string(),
        "--disable-sync".to_string(),
        "--disable-default-apps".to_string(),
        "--no-service-autorun".to_string(),
        "--password-store=basic".to_string(),
        "--use-mock-keychain".to_string(),
        "--enable-automation".to_string(),
    ]
}

/// A JSON value going to or coming from the browser.
pub type Json = serde_json::Value;

/// An HTML document that shows one media file and nothing else.
///
/// A browser already knows how to decode every animated image and video
/// format a deck is likely to carry; what it needs is a document. Generating
/// that document is the whole of what pulpit has to do to play media,
/// which is why it does not carry decoders of its own.
///
/// The element is `object-fit: contain` inside a black page, so the file's
/// own aspect ratio is preserved exactly as the PDF page's letterbox does.
/// Neither page draws any controls, and that is the whole point: the
/// presenter and the audience consume the same frames, so anything painted
/// here is painted on the projector too. A scrub bar that appears whenever
/// the presenter's pointer crosses the video is a scrub bar the room sees.
/// The transport is therefore pulpit's, drawn on the presenter screen
/// only, and it reaches the content through `window.__tp` — which the worker
/// calls, and which is the whole control surface either page exposes.
///
/// What the page owes in return is `__tpReport`: position, duration and mute
/// state, pushed out on the media element's own events, because a transport
/// cannot draw a scrub bar for a playhead it cannot see.
///
/// A click still toggles play/pause, since pointer events only ever come from
/// the presenter and toggling paints nothing. An animated image has no
/// transport to speak of, so its click freezes and unfreezes it.
pub fn wrapper_page(file: &str, video: bool, playback: &WrapperPlayback) -> String {
    let file = escape_attribute(file);
    // Templates rather than `format!`: the page is mostly CSS and JavaScript,
    // and doubling every brace in it invites exactly the sort of silent
    // corruption that is hard to see in a string literal.
    let page = if video { VIDEO_PAGE } else { IMAGE_PAGE };
    let attributes = if video {
        format!(
            "{}{}{}",
            if playback.autoplay { " autoplay" } else { "" },
            if playback.repeat { " loop" } else { "" },
            // Autoplay is refused by every browser unless the media is muted;
            // audio has to be turned on afterwards, from a command.
            if playback.mute || playback.autoplay {
                " muted"
            } else {
                ""
            },
        )
    } else {
        String::new()
    };
    page.replace("__BASE__", BASE_STYLE)
        .replace("__SRC__", &file)
        .replace("__ATTRS__", &attributes)
        .replace("__START__", &playback.start.max(0.0).to_string())
        .replace(
            "__AUTOPLAY__",
            if playback.autoplay { "true" } else { "false" },
        )
}

/// Shared page furniture: a black, unscrollable page with the media centred.
const BASE_STYLE: &str = "html,body{margin:0;height:100%;background:#000;overflow:hidden;\
     font:500 14px system-ui,sans-serif;color:#fff;-webkit-user-select:none;user-select:none}\
     #m,#c{width:100%;height:100%;object-fit:contain;display:block}";

const VIDEO_PAGE: &str = r#"<!doctype html><meta charset=utf-8><style>
__BASE__
</style>
<video id=m src="__SRC__"__ATTRS__ playsinline></video>
<script>
const m=document.getElementById('m');
const start=parseFloat('__START__');
if(start>0){try{m.currentTime=start}catch(e){}}
// The presenter's transport lives in pulpit, so what it needs to draw has
// to come back out of here. Reported on the element's own events rather than
// on a timer: a paused video generates none, and a worker polling for a
// number nobody is watching is a round trip per frame for nothing.
function report(){
  if(!window.__tpReport)return;
  try{
    window.__tpReport(JSON.stringify({
      position:m.currentTime,
      duration:isFinite(m.duration)&&m.duration>0?m.duration:null,
      paused:m.paused,muted:m.muted,volume:m.volume}));
  }catch(e){}
}
function toggle(){if(m.paused){const p=m.play();if(p&&p.catch)p.catch(()=>{});}else{m.pause();}}
// A click on the video still works, and costs the audience nothing: pointer
// events only ever come from the presenter, and this draws no controls.
m.addEventListener('click',toggle);
['play','pause','timeupdate','durationchange','loadedmetadata','seeked','ended','volumechange']
  .forEach(name=>m.addEventListener(name,report));
report();
window.__tp={play:()=>{const p=m.play();if(p&&p.catch)p.catch(()=>{});},
  pause:()=>m.pause(),seek:t=>{m.currentTime=t;report();},
  mute:v=>{m.muted=v},volume:v=>{m.volume=v},loop:v=>{m.loop=v}};
</script>"#;

const IMAGE_PAGE: &str = r#"<!doctype html><meta charset=utf-8><style>
__BASE__
#c{display:none}
</style>
<img id=m src="__SRC__"><canvas id=c></canvas>
<script>
const m=document.getElementById('m'),c=document.getElementById('c');
const base=m.getAttribute('src');
let playing=__AUTOPLAY__,generation=0;
// An <img> has no transport, so "paused" is the current frame copied into a
// canvas, and "playing again" is a fresh request for the file. The query
// parameter is what makes the browser decode it from the first frame rather
// than resume the animation it already has; the asset server ignores it.
// An animation has no playhead, so it reports only whether it is running —
// enough for the presenter's transport to show the right button.
function report(){
  if(!window.__tpReport)return;
  try{
    window.__tpReport(JSON.stringify({
      position:0,duration:null,paused:!playing,muted:true,volume:0}));
  }catch(e){}
}
function freeze(){
  if(!m.complete||!m.naturalWidth){playing=false;report();return;}
  c.width=m.naturalWidth;c.height=m.naturalHeight;
  try{c.getContext('2d').drawImage(m,0,0);}catch(e){return;}
  c.style.display='block';m.style.display='none';playing=false;report();
}
function restart(){
  generation++;
  m.src=base+(base.indexOf('?')<0?'?':'&')+'tp='+generation;
  c.style.display='none';m.style.display='block';playing=true;report();
}
m.addEventListener('load',()=>{if(!playing)freeze();});
if(!playing&&m.complete)freeze();
addEventListener('click',()=>{if(playing)freeze();else restart();});
report();
window.__tp={play:restart,pause:freeze,seek:()=>{},mute:()=>{},volume:()=>{},loop:()=>{}};
</script>"#;

/// What the deck asked for, in the terms the wrapper page understands.
#[derive(Debug, Clone, Copy, Default)]
pub struct WrapperPlayback {
    pub autoplay: bool,
    pub repeat: bool,
    pub mute: bool,
    pub start: f32,
}

/// Escape a value going into an HTML attribute.
///
/// The file name comes from a PDF, so it is untrusted: without this a crafted
/// name could close the attribute and inject markup into the page pulpit
/// generated.
fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A CDP connection over the browser's inherited pipe.
///
/// Messages are NUL-terminated JSON: the browser reads from file descriptor
/// 3 and writes to file descriptor 4. Nothing binds a socket, so no other
/// process on the machine can reach this browser.
/// Ceiling on one CDP message. Screencast frames are the large ones and a
/// 1080p JPEG is far below this; the limit exists to bound a hostile or
/// corrupt stream.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub struct CdpPipe {
    child: Child,
    writer: std::fs::File,
    reader: BufReader<std::fs::File>,
    /// Bytes read but not yet forming a complete NUL-terminated message.
    /// Persistent across calls so a deadline never discards partial input.
    pending: Vec<u8>,
    /// How far into `pending` the terminator search has already looked.
    /// Without this every read re-scanned the buffer from the start, and a
    /// frame arriving in many small pipe chunks made assembling it
    /// quadratic in its size.
    scanned: usize,
    /// Read scratch, reused across calls. Declared on the pipe rather than
    /// the stack: a stack array is re-zeroed on every loop iteration, and at
    /// 250 Hz polling that alone was tens of megabytes a second of memset.
    scratch: Box<[u8]>,
    next_id: u64,
    profile: PathBuf,
}

impl CdpPipe {
    /// Launch a browser and connect to it.
    #[cfg(unix)]
    pub fn launch(
        executable: &Path,
        profile: &Path,
        viewport: Viewport,
        extra_flags: &[String],
    ) -> Result<Self, MediaError> {
        let mut flags = launch_flags(profile, viewport);
        flags.extend_from_slice(extra_flags);
        Self::launch_with_flags(executable, profile, &flags)
    }

    /// Launch with an exact flag list.
    ///
    /// Separated from [`CdpPipe::launch`] so the flag set can be varied when
    /// diagnosing a browser that refuses a command — which flags a build
    /// tolerates is otherwise guesswork.
    #[cfg(unix)]
    pub fn launch_with_flags(
        executable: &Path,
        profile: &Path,
        flags: &[String],
    ) -> Result<Self, MediaError> {
        use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
        use std::os::unix::process::CommandExt;

        let extra_flags = flags;
        for flag in extra_flags {
            if FORBIDDEN_FLAGS.iter().any(|bad| flag.starts_with(bad)) {
                return Err(MediaError::new(
                    MediaErrorKind::PolicyDenied,
                    format!("refusing to launch a browser with {flag}"),
                ));
            }
        }

        // Two pipes: one for each direction. The browser inherits the far end
        // of each as descriptors 3 and 4.
        let (to_browser_read, to_browser_write) = os_pipe()?;
        let (from_browser_read, from_browser_write) = os_pipe()?;

        let mut command = Command::new(executable);
        let mut user_data_dir = std::ffi::OsString::from("--user-data-dir=");
        user_data_dir.push(profile);
        command
            .arg(
                user_data_dir.into_string().unwrap_or_else(|os_str| {
                    format!("--user-data-dir={}", os_str.to_string_lossy())
                }),
            )
            .args(extra_flags)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child_read = to_browser_read.as_raw_fd();
        let child_write = from_browser_write.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                // Chrome expects exactly fd 3 (its input) and fd 4 (its output).
                // After dup2-ing, explicitly clear CLOEXEC. Note: dup2(old, new)
                // only clears CLOEXEC when old != new; when they are equal it is
                // a no-op and the flag is untouched, so we must clear it explicitly.
                if libc_dup2(child_read, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if child_read == 3 {
                    // dup2 was a no-op, clear CLOEXEC manually.
                    libc_fcntl(3, F_SETFD, 0);
                }
                if libc_dup2(child_write, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if child_write == 4 {
                    // dup2 was a no-op, clear CLOEXEC manually.
                    libc_fcntl(4, F_SETFD, 0);
                }
                Ok(())
            });
        }

        let child = command.spawn().map_err(|e| {
            MediaError::new(
                MediaErrorKind::LaunchFailed,
                format!("could not start the browser: {e}"),
            )
        })?;
        drop(to_browser_read);
        drop(from_browser_write);

        let writer = unsafe { std::fs::File::from_raw_fd(to_browser_write.into_raw_fd()) };
        let read_fd = from_browser_read.into_raw_fd();
        // Non-blocking, so `recv`'s deadline means something. Left blocking, a
        // read with nothing to read waits for ever and the worker can neither
        // time out nor go back and serve a command.
        unsafe {
            let flags = libc_fcntl(read_fd, F_GETFL, 0);
            if flags >= 0 {
                libc_fcntl(read_fd, F_SETFL, flags | O_NONBLOCK);
            }
        }
        let reader = BufReader::new(unsafe { std::fs::File::from_raw_fd(read_fd) });

        Ok(Self {
            child,
            writer,
            reader,
            pending: Vec::new(),
            scanned: 0,
            scratch: vec![0u8; 64 * 1024].into_boxed_slice(),
            next_id: 1,
            profile: profile.to_path_buf(),
        })
    }

    #[cfg(not(unix))]
    pub fn launch(
        _executable: &Path,
        _profile: &Path,
        _viewport: Viewport,
        _extra_flags: &[String],
    ) -> Result<Self, MediaError> {
        Err(MediaError::new(
            MediaErrorKind::Unavailable,
            "the debugging-pipe transport is only implemented on unix so far",
        ))
    }

    /// Send a command and return its message identifier.
    pub fn send(&mut self, method: &str, params: Json) -> Result<u64, MediaError> {
        self.send_to_session(method, params, None)
    }

    pub fn send_to_session(
        &mut self,
        method: &str,
        params: Json,
        session: Option<&str>,
    ) -> Result<u64, MediaError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = serde_json::json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(session) = session {
            message["sessionId"] = Json::String(session.to_string());
        }
        let mut encoded = serde_json::to_vec(&message).map_err(|e| {
            MediaError::new(MediaErrorKind::ProtocolViolation, format!("encode: {e}"))
        })?;
        encoded.push(0);
        self.writer.write_all(&encoded).map_err(pipe_error)?;
        self.writer.flush().map_err(pipe_error)?;
        Ok(id)
    }

    /// Read one message, blocking until the deadline.
    pub fn recv(&mut self, deadline: Duration) -> Result<Json, MediaError> {
        let started = Instant::now();
        loop {
            // Anything already read stays in `self.pending` across calls. The
            // first version built the buffer locally and threw it away on
            // timeout, so a screencast frame — tens of kilobytes — could never
            // be assembled within a few milliseconds and no frame ever
            // arrived. Partial reads must survive the deadline.
            if let Some(offset) = self.pending[self.scanned..]
                .iter()
                .position(|byte| *byte == 0)
            {
                let position = self.scanned + offset;
                // Parsed in place: a screencast frame is a couple of hundred
                // kilobytes of base64, and collecting it into its own Vec
                // first was a full copy plus an allocation per message.
                let parsed = serde_json::from_slice(&self.pending[..position]).map_err(|e| {
                    MediaError::new(
                        MediaErrorKind::ProtocolViolation,
                        format!("the browser sent unreadable JSON: {e}"),
                    )
                });
                self.pending.drain(..=position);
                self.scanned = 0;
                return parsed;
            }
            self.scanned = self.pending.len();
            if self.pending.len() > MAX_MESSAGE_BYTES {
                return Err(MediaError::new(
                    MediaErrorKind::ResourceLimit,
                    "the browser sent an implausibly large message",
                ));
            }

            // Chunked, not byte-at-a-time: a syscall per byte cost more than
            // the whole frame budget.
            match self.reader.read(&mut self.scratch) {
                Ok(0) => {
                    return Err(MediaError::new(
                        MediaErrorKind::Crashed,
                        "the browser closed its debugging pipe",
                    ))
                }
                Ok(read) => self.pending.extend_from_slice(&self.scratch[..read]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= deadline {
                        return Err(MediaError::new(
                            MediaErrorKind::TimedOut,
                            "the browser did not answer in time",
                        ));
                    }
                    // Deliberately a sleep, not a poll on the descriptor.
                    // Waking per arriving pipe chunk was measured (bench,
                    // 2026-08-12) to cost 2-3x the worker CPU of letting a
                    // millisecond of chunks batch up per read, for one to
                    // two milliseconds of frame latency nobody can see.
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(pipe_error(e)),
            }
            if started.elapsed() >= deadline {
                return Err(MediaError::new(
                    MediaErrorKind::TimedOut,
                    "the browser did not answer in time",
                ));
            }
        }
    }

    /// Wait for the reply to one command, passing events to `on_event`.
    ///
    /// Events are handed over by value: a screencast frame that lands while
    /// a command is outstanding is a couple of hundred kilobytes, and the
    /// caller files it somewhere anyway — cloning it here would copy every
    /// such frame once per command sent during playback.
    pub fn wait_for(
        &mut self,
        id: u64,
        deadline: Duration,
        mut on_event: impl FnMut(Json),
    ) -> Result<Json, MediaError> {
        let started = Instant::now();
        loop {
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(MediaError::new(
                    MediaErrorKind::TimedOut,
                    "the browser did not answer in time",
                ));
            }
            let message = self.recv(remaining)?;
            if message.get("id").and_then(Json::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(MediaError::new(
                        MediaErrorKind::ProtocolViolation,
                        format!(
                            "the browser refused a command: {}",
                            error.get("message").and_then(Json::as_str).unwrap_or("?")
                        ),
                    ));
                }
                return Ok(message.get("result").cloned().unwrap_or(Json::Null));
            }
            on_event(message);
        }
    }

    /// Confirm the browser implements everything this adapter needs before
    /// any document content is loaded.
    pub fn feature_probe(&mut self, deadline: Duration) -> Result<String, MediaError> {
        let id = self.send("Browser.getVersion", serde_json::json!({}))?;
        let result = self.wait_for(id, deadline, |_| {})?;
        let product = result
            .get("product")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        if !is_chromium_version(&product) {
            return Err(MediaError::new(
                MediaErrorKind::Incompatible,
                format!("the browser reported an unrecognised product string: {product}"),
            ));
        }
        Ok(product)
    }

    pub fn child_id(&self) -> u32 {
        self.child.id()
    }

    /// Close the browser and remove the private profile it was given.
    pub fn shutdown(&mut self) {
        let _ = self.send("Browser.close", serde_json::json!({}));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if let Err(e) = std::fs::remove_dir_all(&self.profile) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    profile = %self.profile.display(),
                    error = %e,
                    "could not remove the browser's private profile"
                );
            }
        }
    }
}

impl Drop for CdpPipe {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn pipe_error(error: std::io::Error) -> MediaError {
    MediaError::new(
        MediaErrorKind::Crashed,
        format!("the browser's debugging pipe failed: {error}"),
    )
}

#[cfg(unix)]
fn os_pipe() -> Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd), MediaError> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a two-element array, which is what pipe2(2) writes.
    // O_CLOEXEC prevents the fds from leaking into child processes.
    let result = unsafe { libc_pipe2(fds.as_mut_ptr(), O_CLOEXEC) };
    if result != 0 {
        return Err(MediaError::new(
            MediaErrorKind::LaunchFailed,
            format!(
                "could not create a debugging pipe: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

// The libc calls this adapter needs, declared directly so the crate does
// not take a dependency for them.
/// `fcntl` constants from `fcntl.h`.
#[cfg(unix)]
const F_GETFL: i32 = 3;
#[cfg(unix)]
const F_SETFL: i32 = 4;
#[cfg(unix)]
const F_SETFD: i32 = 2;
#[cfg(unix)]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(unix)]
const O_CLOEXEC: i32 = 0o2000000;

#[cfg(unix)]
extern "C" {
    #[link_name = "pipe2"]
    fn libc_pipe2(fds: *mut i32, flags: i32) -> i32;
    #[link_name = "fcntl"]
    fn libc_fcntl(fd: i32, command: i32, argument: i32) -> i32;
    #[link_name = "dup2"]
    fn libc_dup2(old: i32, new: i32) -> i32;
}

/// Serve a staged bundle from loopback on an unguessable path.
///
/// Bundle resources are *not* loaded from unrestricted `file://`: a private
/// origin is what lets the CSP, the traversal check and the allowlist mean
/// anything at all.
pub struct AssetServer {
    port: u16,
    /// The unguessable path prefix; a request without it is refused.
    secret: String,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl AssetServer {
    /// Start serving `root`, allowing only the files in `allowlist`.
    /// The path a synthesized wrapper page is served at.
    ///
    /// Deliberately not a name a bundle could contain, so a generated page can
    /// never shadow one the author wrote.
    pub const GENERATED_PAGE: &'static str = "__pulpit__.html";

    pub fn start(root: PathBuf, allowlist: Vec<PathBuf>) -> Result<Self, MediaError> {
        Self::start_with_page(root, allowlist, None)
    }

    /// Serve `root`, plus an optional page held in memory rather than on disk.
    ///
    /// A bare media file — a GIF or a clip — needs an HTML document around it
    /// before a browser can show it. Synthesizing that page here keeps it out
    /// of the user's document directory, which pulpit has no business
    /// writing into.
    pub fn start_with_page(
        root: PathBuf,
        allowlist: Vec<PathBuf>,
        page: Option<String>,
    ) -> Result<Self, MediaError> {
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| {
            MediaError::new(
                MediaErrorKind::LaunchFailed,
                format!("could not open a private asset origin: {e}"),
            )
        })?;
        let port = listener
            .local_addr()
            .map(|address| address.port())
            .map_err(|e| {
                MediaError::new(MediaErrorKind::LaunchFailed, format!("no local port: {e}"))
            })?;
        let secret = unguessable_token()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));

        let thread_secret = secret.clone();
        let thread_shutdown = shutdown.clone();
        let root_clone = root.clone();
        let allowlist_clone = allowlist.clone();
        let page_clone = page.clone();
        let active_connections_clone = active_connections.clone();
        listener.set_nonblocking(true).ok();
        std::thread::Builder::new()
            .name("pulpit-asset-origin".into())
            .spawn(move || {
                const MAX_CONCURRENT: usize = 16;
                while !thread_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).ok();
                            let current = active_connections_clone.load(Ordering::Relaxed);
                            if current >= MAX_CONCURRENT {
                                // At capacity; close immediately rather than queue.
                                drop(stream);
                                continue;
                            }
                            // Increment before spawning so the counter reflects
                            // threads in flight, not just spawned successfully.
                            active_connections_clone.fetch_add(1, Ordering::Relaxed);
                            let secret = thread_secret.clone();
                            let root = root_clone.clone();
                            let allowlist = allowlist_clone.clone();
                            let page = page_clone.clone();
                            let active = active_connections_clone.clone();
                            match std::thread::Builder::new().spawn(move || {
                                // Guard ensures decrement happens even on error or panic.
                                let _guard = ConnectionGuard(active);
                                let _ =
                                    serve_one(stream, &root, &allowlist, &secret, page.as_deref());
                            }) {
                                Ok(_) => {}
                                Err(_) => {
                                    // Spawn failed; decrement the counter manually.
                                    active_connections_clone.fetch_sub(1, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| {
                MediaError::new(
                    MediaErrorKind::LaunchFailed,
                    format!("could not serve the bundle: {e}"),
                )
            })?;

        Ok(Self {
            port,
            secret,
            shutdown,
        })
    }

    /// The URL of one bundle-relative path on the private origin.
    pub fn url_for(&self, relative: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}/{}",
            self.port,
            self.secret,
            relative.trim_start_matches('/')
        )
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for AssetServer {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Guard that decrements the active connection counter when dropped.
/// Ensures cleanup happens even if serve_one errors or panics.
struct ConnectionGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn serve_one(
    mut stream: std::net::TcpStream,
    root: &Path,
    allowlist: &[PathBuf],
    secret: &str,
    generated: Option<&str>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" && method != "HEAD" {
        return respond(&mut stream, 405, "text/plain", b"method not allowed");
    }

    // The rest of the headers, only one of which matters. Bounded so a
    // hostile client cannot make the worker read for ever.
    let mut range_header = None;
    for _ in 0..64 {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range_header = Some(value.trim().to_string());
            }
        }
    }

    // Strip the query, then the unguessable prefix. A request that does not
    // carry the secret does not reach the filesystem check at all.
    let path = target.split(['?', '#']).next().unwrap_or_default();
    let Some(relative) = path
        .strip_prefix('/')
        .and_then(|rest| rest.strip_prefix(secret))
        .map(|rest| rest.trim_start_matches('/'))
    else {
        return respond(&mut stream, 404, "text/plain", b"not found");
    };
    let relative = percent_decode(relative);
    // No directory listings, and no ambiguity about what "" means.
    let relative = if relative.is_empty() {
        "index.html".to_string()
    } else {
        relative
    };
    if relative
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return respond(&mut stream, 403, "text/plain", b"forbidden");
    }

    // The synthesized wrapper lives in memory, not on disk, so it is answered
    // before anything touches the filesystem.
    if let Some(html) = generated {
        if relative == AssetServer::GENERATED_PAGE {
            return respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                html.as_bytes(),
            );
        }
    }

    let candidate = root.join(&relative);
    let allowed = candidate
        .canonicalize()
        .ok()
        .filter(|resolved| allowlist.iter().any(|file| file == resolved));
    let Some(resolved) = allowed else {
        return respond(&mut stream, 404, "text/plain", b"not found");
    };

    let mime = mime_for(&resolved);
    let Ok(file) = std::fs::File::open(&resolved) else {
        return respond(&mut stream, 404, "text/plain", b"not found");
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);

    // Without range support a browser cannot seek a video: it re-requests
    // from byte zero, so dragging the slider moved the picture but playing
    // started again from the beginning. `Accept-Ranges` is what makes the
    // media element treat the resource as seekable at all.
    //
    // The answer is streamed from the file rather than read whole: Chrome
    // probes a video with suffix and open-ended ranges and re-requests on
    // every seek, and reading a movie-sized file end to end to answer a few
    // hundred bytes of index made each of those probes cost the whole file.
    match range_header
        .as_deref()
        .and_then(|header| parse_range(header, length))
    {
        Some((start, end)) => {
            let extra =
                format!("Accept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{length}\r\n");
            respond_file(&mut stream, 206, mime, file, start, end - start + 1, &extra)
        }
        None if range_header.is_some() => {
            let extra = format!("Content-Range: bytes */{length}\r\n");
            respond_with(
                &mut stream,
                416,
                "text/plain",
                b"range not satisfiable",
                &extra,
            )
        }
        None => respond_file(
            &mut stream,
            200,
            mime,
            file,
            0,
            length,
            "Accept-Ranges: bytes\r\n",
        ),
    }
}

/// Stream `length` bytes of `file` from `offset`, without buffering the file
/// in memory.
fn respond_file(
    stream: &mut std::net::TcpStream,
    status: u16,
    mime: &str,
    mut file: std::fs::File,
    offset: u64,
    length: u64,
    extra: &str,
) -> std::io::Result<()> {
    use std::io::{Read, Seek};
    write_response_headers(stream, status, mime, length, extra)?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    std::io::copy(&mut file.take(length), stream)?;
    stream.flush()
}

/// Parse a single-range `bytes=` header against a known length.
///
/// Multipart ranges are deliberately unsupported: no media element asks for
/// one, and answering a request we cannot honour correctly is worse than
/// answering the whole file.
fn parse_range(header: &str, length: u64) -> Option<(u64, u64)> {
    if length == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (start, end) = match (first.trim(), last.trim()) {
        // `bytes=-500`: the final 500 bytes.
        ("", suffix) => {
            let wanted: u64 = suffix.parse().ok()?;
            (length.saturating_sub(wanted.min(length)), length - 1)
        }
        (first, "") => (first.parse().ok()?, length - 1),
        (first, last) => (
            first.parse().ok()?,
            last.parse::<u64>().ok()?.min(length - 1),
        ),
    };
    (start <= end && start < length).then_some((start, end))
}

fn respond(
    stream: &mut std::net::TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
) -> std::io::Result<()> {
    respond_with(stream, status, mime, body, "")
}

fn respond_with(
    stream: &mut std::net::TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    extra: &str,
) -> std::io::Result<()> {
    write_response_headers(stream, status, mime, body.len() as u64, extra)?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_response_headers(
    stream: &mut std::net::TcpStream,
    status: u16,
    mime: &str,
    length: u64,
    extra: &str,
) -> std::io::Result<()> {
    // A deck is self-contained by default: the CSP says so as well as the
    // request-interception layer, because defence in one layer is not a
    // policy.
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {length}\r\n\
         {extra}\
         Content-Security-Policy: default-src 'self' 'unsafe-inline' data: blob:; connect-src 'none'; frame-ancestors 'none'\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
    );
    stream.write_all(headers.as_bytes())
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A token no page and no other local process can guess.
///
/// 128 bits from the OS via `getrandom`, which reads from the secure
/// system entropy source (/dev/urandom on Unix-like systems).
pub fn unguessable_token() -> Result<String, MediaError> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| {
        MediaError::new(
            MediaErrorKind::LaunchFailed,
            format!("could not read the system random source for the asset origin token: {e}"),
        )
    })?;
    Ok(bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_byte_range_is_parsed_the_way_a_media_element_asks_for_one() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
        // Open-ended, which is what Chrome sends first for a video.
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        // Suffix ranges: the last 200 bytes, where an MP4 keeps its index.
        assert_eq!(parse_range("bytes=-200", 1000), Some((800, 999)));
        // An end past the file is clamped rather than refused.
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        // Unsatisfiable or unsupported forms yield nothing, never a panic.
        assert_eq!(parse_range("bytes=1000-1200", 1000), None);
        assert_eq!(parse_range("bytes=0-99,200-299", 1000), None);
        assert_eq!(parse_range("bits=0-99", 1000), None);
        assert_eq!(parse_range("bytes=abc-def", 1000), None);
        assert_eq!(parse_range("bytes=0-499", 0), None);
    }

    #[test]
    fn a_range_request_is_answered_with_partial_content() {
        // Without this a video is not seekable at all: the browser re-fetches
        // from byte zero, so dragging the slider moved the picture but
        // pressing play started again from the beginning.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("clip.mp4"), b"0123456789").unwrap();
        let server = AssetServer::start(root.clone(), vec![root.join("clip.mp4")]).unwrap();
        let response = request(&server, "clip.mp4", Some("bytes=2-5"));
        assert!(response.contains("206 Partial Content"), "{response}");
        assert!(
            response.contains("Content-Range: bytes 2-5/10"),
            "{response}"
        );
        assert!(response.ends_with("2345"), "{response}");

        // And a plain request must advertise that ranges are available.
        let whole = request(&server, "clip.mp4", None);
        assert!(whole.contains("Accept-Ranges: bytes"), "{whole}");
    }

    /// One HTTP request against the private origin.
    fn request(server: &AssetServer, relative: &str, range: Option<&str>) -> String {
        use std::io::Read;
        let url = server.url_for(relative);
        let url = url.strip_prefix("http://").unwrap_or(&url).to_string();
        let (authority, path) = url.split_once('/').unwrap();
        let mut stream = std::net::TcpStream::connect(authority).unwrap();
        let range = range
            .map(|value| format!("Range: {value}\r\n"))
            .unwrap_or_default();
        stream
            .write_all(
                format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\n{range}\r\n").as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn a_video_wrapper_exposes_a_control_surface_but_draws_no_controls() {
        let page = wrapper_page("clip.mp4", true, &WrapperPlayback::default());
        assert!(page.contains("<video"));
        assert!(
            page.contains("window.__tp="),
            "nothing for the host to call"
        );
        assert!(page.contains("seek:t=>{m.currentTime=t"), "no way to seek");
    }

    /// The presenter and the audience share these frames, so a control drawn
    /// in the page is a control on the projector. This is the test that keeps
    /// one from creeping back in.
    #[test]
    fn a_wrapper_page_paints_nothing_the_audience_would_see() {
        for (file, video) in [("clip.mp4", true), ("spin.gif", false)] {
            let page = wrapper_page(file, video, &WrapperPlayback::default());
            for drawn in ["<button", "type=range", "<input", "position:fixed"] {
                assert!(
                    !page.contains(drawn),
                    "{file} draws {drawn}, which the audience would see"
                );
            }
        }
    }

    #[test]
    fn a_wrapper_page_reports_where_playback_has_reached() {
        for (file, video) in [("clip.mp4", true), ("spin.gif", false)] {
            let page = wrapper_page(file, video, &WrapperPlayback::default());
            assert!(
                page.contains("window.__tpReport("),
                "{file} tells the transport nothing"
            );
        }
        let video = wrapper_page("clip.mp4", true, &WrapperPlayback::default());
        assert!(video.contains("position:m.currentTime"));
        assert!(
            video.contains("'timeupdate'"),
            "a playhead that only moves on demand is a playhead nobody can watch"
        );
    }

    #[test]
    fn an_image_wrapper_toggles_between_running_and_frozen() {
        let page = wrapper_page("spin.gif", false, &WrapperPlayback::default());
        assert!(page.contains("<img"));
        assert!(page.contains("if(playing)freeze();else restart();"));
        // Restarting has to re-request the file, or the browser resumes the
        // animation it already has instead of starting from the first frame.
        assert!(page.contains("'tp='+generation"));
    }

    #[test]
    fn playback_intent_reaches_the_generated_document() {
        let page = wrapper_page(
            "clip.mp4",
            true,
            &WrapperPlayback {
                autoplay: true,
                repeat: true,
                mute: false,
                start: 12.5,
            },
        );
        assert!(page.contains(" autoplay"));
        assert!(page.contains(" loop"));
        // Autoplay without muting is refused by every browser, so asking for
        // sound cannot be allowed to cost the deck its autostart.
        assert!(page.contains(" muted"));
        assert!(page.contains("parseFloat('12.5')"));
    }

    #[test]
    fn a_hostile_file_name_cannot_break_out_of_the_generated_document() {
        let page = wrapper_page(
            "evil\" onerror=\"alert(1)",
            false,
            &WrapperPlayback::default(),
        );
        assert!(!page.contains("onerror=\"alert"));
        assert!(page.contains("&quot;"));
    }

    #[test]
    fn a_chromium_version_banner_is_recognised_and_others_are_not() {
        assert!(is_chromium_version("Google Chrome 140.0.7339.80"));
        assert!(is_chromium_version("Chromium 139.0.6900.0"));
        assert!(is_chromium_version("Microsoft Edge 140.0.3485.14"));
        assert!(is_chromium_version("Brave Browser 1.70.117"));
        assert!(!is_chromium_version("Mozilla Firefox 131.0"));
        assert!(!is_chromium_version("Safari 18.0"));
        assert!(!is_chromium_version("some other program"));
        assert!(
            !is_chromium_version("chrome"),
            "a name without a version number is not a version banner"
        );
    }

    #[test]
    fn the_launch_flags_isolate_the_browser_from_the_users_own_profile() {
        let flags = launch_flags(
            Path::new("/tmp/private-profile"),
            Viewport::new(1280, 720, 1.0),
        );
        assert!(flags.iter().any(|flag| flag == "--remote-debugging-pipe"));
        assert!(flags
            .iter()
            .any(|flag| flag == "--user-data-dir=/tmp/private-profile"));
        assert!(flags.iter().any(|flag| flag == "--disable-extensions"));
        assert!(flags.iter().any(|flag| flag.starts_with("--headless")));
    }

    #[test]
    fn no_launch_flag_weakens_the_browser_sandbox() {
        let flags = launch_flags(Path::new("/tmp/p"), Viewport::new(1280, 720, 1.0));
        for flag in &flags {
            assert!(
                !FORBIDDEN_FLAGS.iter().any(|bad| flag.starts_with(bad)),
                "{flag} must never appear"
            );
        }
    }

    #[test]
    fn the_viewport_scale_reaches_the_browser_as_css_pixels() {
        let flags = launch_flags(Path::new("/tmp/p"), Viewport::new(2560, 1440, 2.0));
        assert!(
            flags.iter().any(|flag| flag == "--window-size=1280,720"),
            "a 2× viewport is 1280×720 CSS pixels: {flags:?}"
        );
        assert!(flags
            .iter()
            .any(|flag| flag == "--force-device-scale-factor=2"));
    }

    #[test]
    fn an_unguessable_token_is_long_and_differs_between_calls() {
        let first = unguessable_token().expect("failed to generate token");
        let second = unguessable_token().expect("failed to generate token");
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mime_types_are_declared_rather_than_sniffed_by_the_browser() {
        assert_eq!(
            mime_for(Path::new("a/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_for(Path::new("a/app.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for(Path::new("a/x.png")), "image/png");
        assert_eq!(
            mime_for(Path::new("a/unknown.xyz")),
            "application/octet-stream"
        );
    }

    #[test]
    fn percent_escapes_are_decoded_before_the_traversal_check() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("plain.html"), "plain.html");
        assert_eq!(percent_decode("%2e%2e"), "..");
    }

    #[test]
    fn the_asset_server_serves_only_allowlisted_files_behind_its_secret() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("index.html"), b"<h1>hello</h1>").unwrap();
        std::fs::write(root.join("secret.txt"), b"not for the page").unwrap();
        let allowed = vec![root.join("index.html").canonicalize().unwrap()];

        let server = AssetServer::start(root.clone(), allowed).unwrap();
        let fetch = |url: &str| -> String {
            let url = url.trim_start_matches("http://");
            let (authority, path) = url.split_once('/').unwrap_or((url, ""));
            let mut stream = std::net::TcpStream::connect(authority).unwrap();
            stream
                .write_all(format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
                .unwrap();
            let mut response = String::new();
            stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
            let _ = stream.read_to_string(&mut response);
            response
        };

        assert!(
            fetch(&server.url_for("index.html")).contains("hello"),
            "an allowlisted file is served"
        );
        assert!(
            fetch(&server.url_for("secret.txt")).contains("404"),
            "a staged file that is not allowlisted is not served"
        );
        assert!(
            fetch(&format!("{}/index.html", server.origin())).contains("404"),
            "a request without the secret prefix is refused"
        );
        assert!(
            fetch(&server.url_for("../secret.txt")).contains("40"),
            "traversal is refused"
        );
    }

    #[test]
    fn the_asset_server_sets_a_restrictive_content_security_policy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("index.html"), b"<h1>hi</h1>").unwrap();
        let allowed = vec![root.join("index.html").canonicalize().unwrap()];
        let server = AssetServer::start(root, allowed).unwrap();

        let url = server.url_for("index.html");
        let url = url.trim_start_matches("http://");
        let (authority, path) = url.split_once('/').unwrap();
        let mut stream = std::net::TcpStream::connect(authority).unwrap();
        stream
            .write_all(format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = String::new();
        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
        let _ = stream.read_to_string(&mut response);

        assert!(response.contains("Content-Security-Policy"));
        assert!(
            response.contains("connect-src 'none'"),
            "network access is denied by the policy as well as by interception"
        );
        assert!(response.contains("X-Content-Type-Options: nosniff"));
    }

    #[test]
    fn every_domain_the_adapter_needs_is_named_so_it_can_be_probed() {
        for domain in ["Page", "Input", "Runtime", "Fetch", "Emulation"] {
            assert!(REQUIRED_DOMAINS.contains(&domain));
        }
    }
}
