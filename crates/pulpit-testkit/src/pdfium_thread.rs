//! One thread for every PDFium form-fill call in a test binary.
//!
//! The pinned PDFium is a V8 build. PDFium creates its V8 isolate lazily —
//! `FORM_OnAfterLoadPage` is what triggers it, so *any* form-fill work does,
//! not only a document that carries scripts — and the isolate belongs to the
//! thread that created it. Touching it from a second thread is a segmentation
//! fault inside V8's snapshot deserialiser: no error return, no unwind, the
//! whole test binary dies and takes the results of the tests that already
//! passed with it.
//!
//! libtest gives every `#[test]` its own thread, including under
//! `--test-threads=1`, so a suite with two form-fill tests in it crashes
//! however it is invoked. That is a property of the harness rather than of
//! pulpit: the document worker's `serve` loop is a synchronous
//! read-handle-write on the worker process's main thread, so a running pulpit
//! only ever calls PDFium from one thread.
//!
//! This restores that property for tests. Bodies are handed to one long-lived
//! thread and run there, one at a time, in the order they arrive.

use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

type Job = Box<dyn FnOnce() + Send + 'static>;

fn jobs() -> &'static Sender<Job> {
    static JOBS: OnceLock<Sender<Job>> = OnceLock::new();
    JOBS.get_or_init(|| {
        let (send, receive) = channel::<Job>();
        std::thread::Builder::new()
            .name("pdfium".into())
            // PDFium's own stack use plus V8's is more than the 2 MiB a
            // spawned thread gets by default on some platforms, and a stack
            // overflow here would look exactly like the crash this module
            // exists to prevent.
            .stack_size(16 << 20)
            .spawn(move || {
                for job in receive {
                    job();
                }
            })
            .expect("the PDFium test thread starts");
        send
    })
}

/// Run `work` on the one thread this test binary calls PDFium from.
///
/// Panics — which is what a failed assertion inside `work` is — are carried
/// back and re-raised on the calling thread, so a failure is reported against
/// the test that caused it and the shared thread survives to run the next one.
pub fn on_the_pdfium_thread<T, F>(work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (send, receive) = channel();
    jobs()
        .send(Box::new(move || {
            // `AssertUnwindSafe` because the closure is a test body: it owns
            // what it touches, and a panic partway through it is the failure
            // being reported rather than state anyone reads afterwards.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            // A receiver that has gone away means the calling test thread is
            // already unwinding; there is nobody left to tell.
            let _ = send.send(result);
        }))
        .expect("the PDFium test thread is running");
    match receive.recv() {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("the PDFium test thread died"),
    }
}
