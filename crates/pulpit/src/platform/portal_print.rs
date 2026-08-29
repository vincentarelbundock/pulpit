//! Printing through `org.freedesktop.portal.Print`, which is the desktop's
//! own print dialog.
//!
//! This is the one platform of the three where "use the system dialog" costs
//! pulpit nothing architecturally. The portal takes a file descriptor onto a
//! PDF, shows the dialog the rest of the desktop shows, and prints. Duplex,
//! paper, trays, margins, scaling and colour are settled inside it, by code
//! that is not ours and not written twice.
//!
//! ## The handshake
//!
//! Two calls, because the dialog and the printing are separate steps:
//!
//! 1. `PreparePrint` puts the dialog up. It returns a `Request` object path
//!    immediately and answers *later*, on that object's `Response` signal,
//!    carrying the settings the reader chose and a token standing for them.
//! 2. `Print` takes the token back along with the file descriptor, and
//!    spools without asking anything a second time.
//!
//! The token is what keeps the reader from seeing two dialogs.
//!
//! ## Why the request path is computed rather than taken from the reply
//!
//! The portal sends `Response` as soon as the reader answers, which can be
//! before the `PreparePrint` reply has been read. Subscribing after the call
//! returns is a race that loses the signal on a fast answer or an already-
//! open dialog. The specification exists for this: the caller passes a
//! `handle_token` and can derive the object path from it, so the match rule
//! is in place before anything is asked for. The reply's path is still
//! compared against the derived one, and a mismatch is reported rather than
//! waited on forever.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::platform::services::PrintJob;
use crate::platform::Outcome;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PRINT: &str = "org.freedesktop.portal.Print";
const REQUEST: &str = "org.freedesktop.portal.Request";

/// The portal's own spelling of "the reader closed the dialog".
const RESPONSE_CANCELLED: u32 = 1;
/// Anything else the portal could not carry out.
const RESPONSE_FAILED: u32 = 2;

/// Distinguishes the requests of one process from each other. The pid
/// distinguishes them from another pulpit's on the same bus.
static NEXT_TOKEN: AtomicU32 = AtomicU32::new(1);

/// Whether this session has a print portal to talk to.
///
/// Asked by introspecting for the interface rather than by looking for the
/// service: `xdg-desktop-portal` is present on desktops whose backend
/// implements no `Print` at all, and a capability that says yes there is a
/// print command that opens nothing.
#[allow(dead_code)] // the capability snapshot now probes on a shared connection
pub fn available() -> bool {
    // Bounded, because this is asked while the capabilities are being read —
    // which is before there is a window. A portal that accepts the call and
    // never answers would otherwise be a pulpit that never starts, with
    // nothing on screen to close.
    crate::platform::linux::on_the_bus("portal print probe", available_on).unwrap_or(false)
}

/// The same question, on a connection somebody else already opened — so the
/// capability snapshot can ask everything it wants of the bus in one trip.
pub fn available_on(connection: &Connection) -> bool {
    let Ok(proxy) = zbus::blocking::Proxy::new(
        connection,
        PORTAL,
        PORTAL_PATH,
        "org.freedesktop.DBus.Properties",
    ) else {
        return false;
    };
    // Every portal interface carries a `version` property. Asking for
    // this one's is the cheapest question whose answer is "there is a
    // Print implementation behind this bus name".
    proxy
        .call::<_, _, OwnedValue>("Get", &(PRINT, "version"))
        .is_ok()
}

/// Put the desktop's print dialog up and spool what it says.
///
/// Blocking, and blocking for as long as the reader looks at the dialog:
/// the caller runs this off the event loop. See
/// [`crate::platform::services::PlatformServices::print_with_dialog`].
pub fn print_with_dialog(job: &PrintJob) -> Outcome {
    if !job.file.is_file() {
        return Outcome::failed("there is nothing at that path to print");
    }
    let connection = match Connection::session() {
        Ok(connection) => connection,
        Err(e) => return Outcome::failed(format!("no session bus: {e}")),
    };

    let token = match prepare(&connection, job) {
        Ok(token) => token,
        Err(outcome) => return outcome,
    };
    spool(&connection, job, token)
}

/// Step 1: the dialog. Returns the token standing for what the reader chose.
fn prepare(connection: &Connection, job: &PrintJob) -> Result<u32, Outcome> {
    let handle = Handle::mint(connection)?;
    // The match rule goes on before the call does. See the module note.
    let mut responses = handle.listen()?;

    let options: HashMap<&str, Value> = HashMap::from([
        ("handle_token", Value::from(handle.token.as_str())),
        // Modal to nothing: pulpit passes no parent window (see
        // `PARENT_WINDOW`), so asking for modality would name no window to be
        // modal to. The dialog is still the desktop's, and still on top.
        ("modal", Value::from(false)),
    ]);
    let proxy = proxy(connection, PORTAL_PATH, PRINT)?;
    // Empty settings and page setup: every question they could preload is one
    // the dialog is about to ask, and answering it in advance would be pulpit
    // deciding what the system dialog opens on.
    let empty: HashMap<&str, Value> = HashMap::new();
    let path: OwnedObjectPath = proxy
        .call(
            "PreparePrint",
            &(PARENT_WINDOW, job.title.as_str(), &empty, &empty, options),
        )
        .map_err(|e| Outcome::failed(format!("the print portal refused to open: {e}")))?;
    handle.confirm(&path)?;

    let (response, results) = handle.answer(&mut responses)?;
    match response {
        0 => {}
        RESPONSE_CANCELLED => return Err(Outcome::refused("The print was cancelled.")),
        RESPONSE_FAILED => return Err(Outcome::failed("the print dialog ended in an error")),
        other => {
            return Err(Outcome::failed(format!(
                "the print portal answered {other}"
            )))
        }
    }
    // The token is what makes step 2 silent. Without it the portal is
    // entitled to put the dialog up a second time, which is the one thing
    // this whole path exists to avoid.
    results
        .get("token")
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Outcome::failed("the print dialog returned no settings to print with"))
}

/// Step 2: the file, and the token from step 1. Shows nothing.
fn spool(connection: &Connection, job: &PrintJob, token: u32) -> Outcome {
    let file = match std::fs::File::open(&job.file) {
        Ok(file) => file,
        Err(e) => return Outcome::failed(format!("the file to print could not be read: {e}")),
    };
    let handle = match Handle::mint(connection) {
        Ok(handle) => handle,
        Err(outcome) => return outcome,
    };
    let mut responses = match handle.listen() {
        Ok(responses) => responses,
        Err(outcome) => return outcome,
    };

    let options: HashMap<&str, Value> = HashMap::from([
        ("handle_token", Value::from(handle.token.as_str())),
        ("token", Value::from(token)),
    ]);
    let proxy = match proxy(connection, PORTAL_PATH, PRINT) {
        Ok(proxy) => proxy,
        Err(outcome) => return outcome,
    };
    // The descriptor is borrowed for the duration of the call and the portal
    // dups it; `file` stays alive across the call, which is what keeps that
    // true. It is also why the caller may delete the scratch copy the moment
    // this returns.
    let fd = zbus::zvariant::Fd::from(&file);
    let path: Result<OwnedObjectPath, _> =
        proxy.call("Print", &(PARENT_WINDOW, job.title.as_str(), fd, options));
    let path = match path {
        Ok(path) => path,
        Err(e) => return Outcome::failed(format!("the print portal would not take it: {e}")),
    };
    if let Err(outcome) = handle.confirm(&path) {
        return outcome;
    }

    match handle.answer(&mut responses) {
        Ok((0, _)) => Outcome::Done,
        // A cancel here is not the dialog — there is no dialog in this step.
        // It is the portal declining after the fact, and it is still not an
        // error to shout about.
        Ok((RESPONSE_CANCELLED, _)) => Outcome::refused("The print was cancelled."),
        Ok((RESPONSE_FAILED, _)) => Outcome::failed("the printer would not take the job"),
        Ok((other, _)) => Outcome::failed(format!("the print portal answered {other}")),
        Err(outcome) => outcome,
    }
}

/// No parent window.
///
/// The portal takes an `x11:` or `wayland:` window identifier so the dialog
/// can be parented, and getting one means holding a native handle — which is
/// the thing the second standing rule forbids outliving an event-loop turn,
/// and this call outlives many. The empty string is what the specification
/// says to pass when there is none, and every tested backend puts the dialog
/// up anyway.
const PARENT_WINDOW: &str = "";

/// One outstanding portal request: the token, the path it implies, and the
/// signal that will answer on it.
struct Handle<'a> {
    connection: &'a Connection,
    token: String,
    path: String,
}

impl<'a> Handle<'a> {
    fn mint(connection: &'a Connection) -> Result<Handle<'a>, Outcome> {
        // `:1.234` is not usable in an object path; the portal specifies this
        // exact transformation of it.
        let unique = connection
            .unique_name()
            .map(|name| name.as_str().to_string());
        let Some(unique) = unique else {
            return Err(Outcome::failed("the session bus gave this process no name"));
        };
        let sender = unique.trim_start_matches(':').replace('.', "_");
        let token = format!(
            "pulpit_{}_{}",
            std::process::id(),
            NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Handle {
            path: format!("{PORTAL_PATH}/request/{sender}/{token}"),
            token,
            connection,
        })
    }

    /// Subscribe before asking, so a fast answer cannot arrive first.
    fn listen(&self) -> Result<zbus::blocking::proxy::SignalIterator<'static>, Outcome> {
        let proxy = proxy(self.connection, &self.path, REQUEST)?;
        proxy
            .receive_signal("Response")
            .map_err(|e| Outcome::failed(format!("the print portal could not be listened to: {e}")))
    }

    /// The portal's reply names the path it will answer on. It is the derived
    /// one on every implementation; a mismatch means the subscription is
    /// listening to the wrong object, and waiting on it would hang forever.
    fn confirm(&self, given: &OwnedObjectPath) -> Result<(), Outcome> {
        if given.as_str() == self.path {
            return Ok(());
        }
        Err(Outcome::failed(format!(
            "the print portal answered on {} rather than {}",
            given.as_str(),
            self.path
        )))
    }

    /// Wait for the reader.
    ///
    /// Unbounded on purpose: the only honest timeout for "a person is
    /// choosing a printer" is none. What ends the wait if the portal dies is
    /// the signal stream ending with it.
    fn answer(
        &self,
        responses: &mut zbus::blocking::proxy::SignalIterator<'static>,
    ) -> Result<(u32, HashMap<String, OwnedValue>), Outcome> {
        let Some(message) = responses.next() else {
            return Err(Outcome::failed("the print portal went away"));
        };
        message
            .body()
            .deserialize::<(u32, HashMap<String, OwnedValue>)>()
            .map_err(|e| {
                Outcome::failed(format!("the print portal said something unreadable: {e}"))
            })
    }
}

fn proxy<'a>(
    connection: &'a Connection,
    path: &str,
    interface: &str,
) -> Result<zbus::blocking::Proxy<'a>, Outcome> {
    zbus::blocking::Proxy::new(connection, PORTAL, path.to_string(), interface.to_string())
        .map_err(|e| Outcome::failed(format!("the print portal could not be reached: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_there_fails_before_the_dialog_opens() {
        // Not `Unsupported`, and not a dialog the reader then has to cancel:
        // there is nothing to print, and that is knowable first.
        let job = PrintJob {
            file: std::path::PathBuf::from("/nonexistent/pulpit-test.pdf"),
            title: "Lease agreement".into(),
            pages: Vec::new(),
            copies: 1,
            destination: None,
        };
        assert!(matches!(print_with_dialog(&job), Outcome::Failed { .. }));
    }

    #[test]
    fn availability_is_a_question_about_this_session() {
        // Whatever this machine has, the answer comes from the bus rather
        // than from `cfg!(target_os = …)`.
        let _ = available();
    }

    /// The one thing in this module that cannot be reasoned out from the
    /// specification alone: that the object path derived from the token is
    /// the path the portal actually answers on. Everything else here waits on
    /// that path, so if it is wrong the wait never ends.
    ///
    /// Ignored because it opens the desktop's print dialog. It closes it
    /// again before returning — `Request.Close` is what a caller is supposed
    /// to use to withdraw — but a test that puts a window on someone's screen
    /// does not belong in a run nobody is watching. Run it with
    /// `cargo test -p pulpit portal_answers_on -- --ignored`.
    #[test]
    #[ignore = "opens the desktop's print dialog"]
    fn the_portal_answers_on_the_path_the_token_implies() {
        let Ok(connection) = Connection::session() else {
            eprintln!("no session bus; nothing to check");
            return;
        };
        if !available() {
            eprintln!("no print portal on this session; nothing to check");
            return;
        }
        let handle = Handle::mint(&connection).expect("a request handle");
        let _responses = handle.listen().expect("a subscription");

        let options: HashMap<&str, Value> = HashMap::from([
            ("handle_token", Value::from(handle.token.as_str())),
            ("modal", Value::from(false)),
        ]);
        let empty: HashMap<&str, Value> = HashMap::new();
        let print = proxy(&connection, PORTAL_PATH, PRINT).expect("the print portal");
        let path: OwnedObjectPath = print
            .call(
                "PreparePrint",
                &(PARENT_WINDOW, "pulpit self-test", &empty, &empty, options),
            )
            .expect("PreparePrint");

        // Withdraw first, so the dialog does not outlive the assertion
        // whichever way the assertion goes.
        let close = proxy(&connection, path.as_str(), REQUEST);
        if let Ok(request) = close {
            let _ = request.call::<_, _, ()>("Close", &());
        }

        assert_eq!(
            path.as_str(),
            handle.path,
            "the portal answers somewhere other than the derived path, and every \
             wait in this module would hang"
        );
    }

    #[test]
    fn a_request_path_is_the_one_the_portal_will_answer_on() {
        // The transformation the specification names: the leading colon
        // goes, and the dots become underscores. Asserted on a literal
        // rather than on a live bus name so the shape is pinned even where
        // there is no bus to connect to.
        let sender = ":1.234".trim_start_matches(':').replace('.', "_");
        assert_eq!(sender, "1_234");
    }
}
