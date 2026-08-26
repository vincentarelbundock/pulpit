//! Printing through CUPS, which is the spooler on Linux and on macOS alike.
//!
//! Both Unix adapters reach a printer the same way, so they reach it here
//! rather than each keeping its own copy of the argument list. What is
//! platform-specific about printing on those two systems is nothing: `lp`
//! and `lpstat` are the interface, and the differences that do exist — which
//! queues are configured, what the default is — are answers CUPS gives, not
//! branches this code takes.
//!
//! ## Why `lp` and not the portal
//!
//! `org.freedesktop.portal.Print` is the better answer on Linux and is not
//! what runs here yet. It is a two-call handshake: `PreparePrint` puts up the
//! system dialog and answers on a `Response` signal, then `Print` takes the
//! settings back along with a file descriptor. The Linux adapter has no
//! machinery for waiting on a portal response at all — every portal call it
//! makes today is fire-and-forget — and writing that handshake is its own
//! piece of work. It changes nothing above this module when it lands: the
//! views ask [`crate::platform::Capabilities::printing`], and `lp` stays as
//! the fallback for a session that has CUPS and no portal.

use std::process::{Command, Stdio};

use crate::platform::services::PrintJob;
use crate::platform::Outcome;

/// Whether there is a spooler here to hand a file to.
///
/// Asked of the path rather than of the operating system: a container with
/// no CUPS in it is a session that cannot print, whatever it is running on.
pub fn available() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|directory| directory.join("lp").is_file()))
        .unwrap_or(false)
}

/// The queues CUPS knows about, in the order it lists them.
///
/// `lpstat -e` rather than `-a`: it names every destination the session can
/// reach, including the ones discovered over the network that have not been
/// made local, which is what a reader looking for the printer down the
/// corridor is looking for.
pub fn printers() -> Vec<String> {
    let Ok(output) = Command::new("lpstat")
        .arg("-e")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Hand the file to CUPS, and wait for `lp` to be done with it.
///
/// Waiting rather than spawning: the file may be a scratch copy the caller
/// deletes as soon as this returns, and `lp` reads it before it queues it.
/// Waiting is also what turns "the printer or class does not exist" into an
/// [`Outcome::Failed`] the reader sees, instead of a message on a stderr
/// nobody is attached to.
pub fn print(job: &PrintJob) -> Outcome {
    if !job.file.is_file() {
        return Outcome::failed("there is nothing at that path to print");
    }
    let mut arguments: Vec<String> = Vec::new();
    if let Some(destination) = job.destination.as_deref() {
        arguments.push("-d".into());
        arguments.push(destination.to_string());
    }
    arguments.push("-n".into());
    arguments.push(job.copies.max(1).to_string());
    // What the queue calls the job. `-t` and not the file name, so a scratch
    // copy never shows the reader "(to print 4213)" in their print queue.
    arguments.push("-t".into());
    arguments.push(job.title.clone());
    if let Some(range) = job.cups_range() {
        // `-o page-ranges=` is the documented CUPS option. The System V `-P`
        // spelling means something else on some builds, which is exactly the
        // kind of difference that shows up as forty pages of paper.
        arguments.push("-o".into());
        arguments.push(format!("page-ranges={range}"));
    }
    // Everything after this is a file name, however it begins. Without it a
    // document called `-d something.pdf` is an argument.
    arguments.push("--".into());
    arguments.push(job.file.to_string_lossy().into_owned());

    let output = match Command::new("lp")
        .args(&arguments)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Outcome::Unsupported { what: "Printing" }
        }
        Err(e) => return Outcome::failed(e.to_string()),
    };
    if output.status.success() {
        return Outcome::Done;
    }
    // `lp`'s own words, which say something useful far more often than an
    // exit code does.
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    if said.is_empty() {
        Outcome::failed(format!("the spooler {}", output.status))
    } else {
        Outcome::failed(said.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn job() -> PrintJob {
        PrintJob {
            file: PathBuf::from("/nonexistent/pulpit-test.pdf"),
            title: "Lease agreement".into(),
            pages: Vec::new(),
            copies: 1,
            destination: None,
        }
    }

    #[test]
    fn a_file_that_is_not_there_fails_before_anything_is_spooled() {
        // Not `Unsupported`: the spooler is fine, the file is not, and a
        // reader told "printing is not available in this session" would go
        // looking in the wrong place.
        assert!(matches!(print(&job()), Outcome::Failed { .. }));
    }

    #[test]
    fn the_range_a_job_carries_is_the_one_cups_reads() {
        let mut job = job();
        assert_eq!(job.cups_range(), None);
        job.pages = vec![1..=3, 7..=7];
        assert_eq!(job.cups_range().as_deref(), Some("1-3,7"));
    }

    #[test]
    fn availability_is_a_question_about_the_path() {
        // Whatever this machine has, the answer comes from PATH rather than
        // from `cfg!(target_os = …)` — which is the rule this module exists
        // to keep out of the adapters.
        let _ = available();
    }
}
