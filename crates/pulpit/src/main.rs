//! pulpit: A Snappy and Snazzy PDF Projector.
//!
//! One executable, several roles. Run normally it is the presenter
//! application; run with `--render-worker` it is a renderer worker process,
//! and with `--document-worker=FILE` it is a document worker holding one open
//! PDF. Every role is this same binary re-executed with a flag, which is how
//! a supervisor spawns what it needs without a second installed binary to
//! ship, sign or find on `PATH`.

mod app;
mod coalesce;
mod datefield;
mod designer;
mod designer_view;
mod display;
mod doc;
mod form_flow;
mod latency;
mod layout;
mod layout_renderer;
mod media;
mod panel;
mod platform;
mod reader;
mod reader_journal;
mod reader_link;
mod residency;
mod session;
mod settings;
mod signature_profiles;
mod signing;
mod theme;
mod thumbnails;
mod toast;
mod typst_annotation;
mod vendor;
mod view;
mod widgets;

use std::path::PathBuf;

use crate::settings::diagnostics::Logging;

/// Which page the window opens on. `--layouts` and `--edit-layout` exist so
/// the designer can be reached directly, including from a desktop launcher.
#[derive(Debug, Clone, PartialEq)]
pub enum StartPage {
    Presenter,
    Library,
    Editor(String),
}

fn main() -> iced::Result {
    use std::ffi::OsStr;

    let mut arguments = std::env::args_os().skip(1);
    let mut document: Option<PathBuf> = None;
    let mut worker = false;
    let mut restore_interrupted_session = true;
    let mut start_page = StartPage::Presenter;
    while let Some(argument) = arguments.next() {
        // `as_encoded_bytes` rather than the Unix-only `OsStrExt::as_bytes`:
        // a path the shell hands us need not be UTF-8 on any platform, and
        // this is the one view of an `OsStr`'s bytes every platform offers.
        let arg_bytes = argument.as_encoded_bytes();
        if argument == "--render-worker" {
            worker = true;
        } else if arg_bytes.starts_with(b"--document-worker=") {
            // Extract the path part after the '=' without UTF-8 conversion
            let path_bytes = &arg_bytes[b"--document-worker=".len()..];
            // SAFETY: `path_bytes` is a suffix of bytes that came from
            // `as_encoded_bytes`, split immediately after an ASCII '='.
            // Splitting on an ASCII boundary is exactly the case
            // `from_encoded_bytes_unchecked` documents as valid.
            let path = PathBuf::from(unsafe { OsStr::from_encoded_bytes_unchecked(path_bytes) });
            run_document_worker(path);
            return Ok(());
        } else if argument == "--typst-worker" {
            crate::typst_annotation::run_worker();
            return Ok(());
        } else if arg_bytes.starts_with(b"--media-worker=") {
            // Extract the role string and validate it's UTF-8 since run_role expects it
            let role_bytes = &arg_bytes[b"--media-worker=".len()..];
            match std::str::from_utf8(role_bytes) {
                Ok(role) => {
                    if !pulpit_media::worker::run_role(role) {
                        eprintln!("unknown media worker role: {role}");
                        std::process::exit(2);
                    }
                    return Ok(());
                }
                Err(_) => {
                    eprintln!("media worker role is not valid UTF-8");
                    std::process::exit(2);
                }
            }
        } else if argument == "--layouts" {
            start_page = StartPage::Library;
        } else if argument == "--no-restore" {
            restore_interrupted_session = false;
        } else if argument == "--edit-layout" {
            start_page = match arguments.next() {
                Some(id) => match id.into_string() {
                    Ok(id_str) => StartPage::Editor(id_str),
                    Err(_) => {
                        eprintln!("--edit-layout argument is not valid UTF-8");
                        std::process::exit(2);
                    }
                },
                None => {
                    eprintln!("--edit-layout needs a layout id");
                    std::process::exit(2);
                }
            }
        } else if argument == "--help" || argument == "-h" {
            print_help();
            return Ok(());
        } else if argument == "--version" || argument == "-V" {
            println!("pulpit {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        } else {
            // This is the document file path, keep it as OsString
            document = Some(PathBuf::from(argument));
        }
    }

    if worker {
        run_worker();
        return Ok(());
    }

    let settings = crate::settings::load_or_default();
    let _logging = Logging::init(
        &settings.diagnostics.level,
        settings.diagnostics.persistent_log,
    );
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "pulpit starting");

    // One presenter per machine. A second copy would open a second audience
    // window on the same projector and the two would fight for the screen.
    let directories = crate::platform::Directories::detect();
    let _instance = match crate::platform::acquire_instance(&directories.instance_lock()) {
        crate::platform::Instance::Acquired(lock) => Some(lock),
        crate::platform::Instance::AlreadyRunning { pid, lock } => {
            match pid {
                Some(pid_num) => {
                    eprintln!(
                        "pulpit is already running (process {pid_num}).\n\
                         Switch to that window instead — a second copy would open a second\n\
                         audience window and the two would flicker against each other.\n\
                         If that process is gone, delete {}.",
                        lock.display()
                    );
                    tracing::warn!(pid_num, "refused to start a second instance");
                }
                None => {
                    eprintln!(
                        "pulpit is already running (process id unavailable).\n\
                         Switch to that window instead — a second copy would open a second\n\
                         audience window and the two would flicker against each other.\n\
                         If that process is gone, delete {}.",
                        lock.display()
                    );
                    tracing::warn!("refused to start a second instance");
                }
            }
            return Ok(());
        }
        crate::platform::Instance::Unknown { reason } => {
            tracing::warn!(reason, "could not record the single-instance claim");
            None
        }
    };

    // Iced panics rather than returning an error when the event loop cannot
    // be created, which is precisely the missing-library case. Catch it in a
    // hook so the user gets an explanation rather than a backtrace.
    install_startup_panic_hook();

    let daemon = iced::daemon(
        move || {
            app::App::new(
                document.clone(),
                start_page.clone(),
                settings.clone(),
                restore_interrupted_session,
            )
        },
        app::App::update,
        view::view,
    );
    // Typst already bundles DejaVu Sans Mono for annotations. Register those
    // same licensed assets with Iced so technical readouts are identical on
    // every platform without adding another font payload to the binary.
    let daemon = typst_assets::fonts().fold(daemon, |daemon, font| daemon.font(font));
    let result = daemon
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .run();

    if let Err(e) = &result {
        explain_startup_failure(e);
    }
    result
}

fn install_startup_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| info.payload().downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        if message.contains("Create event loop") || message.contains("cannot open shared object") {
            explain_missing_libraries(&message);
            // The explanation is the useful part; the backtrace is not.
            std::process::exit(1);
        }
        previous(info);
    }));
}

/// Turn a raw toolkit startup error into something actionable.
///
/// The common failure is not a bug in pulpit: winit, wgpu and PDFium all
/// `dlopen` their libraries at run time, so on a distribution without a global
/// library path (NixOS, or a minimal container) a perfectly good binary exits
/// before it can open a window. Say which library is missing and how to fix
/// it, instead of printing a debug-formatted panic.
fn explain_startup_failure(error: &iced::Error) {
    explain_missing_libraries(&error.to_string());
}

fn explain_missing_libraries(text: &str) {
    let missing: Vec<&str> = [
        "libXcursor",
        "libxkbcommon",
        "libwayland",
        "libvulkan",
        "libGL",
    ]
    .into_iter()
    .filter(|library| text.contains(library))
    .collect();

    eprintln!("\npulpit could not start a graphical session.");
    if !missing.is_empty() {
        eprintln!(
            "\nThese libraries are loaded at run time and were not found: {}",
            missing.join(", ")
        );
        eprintln!(
            "\nThey are not linked into the binary, so the build succeeded and the launch\n\
             did not. Fixes, in order of preference:\n"
        );
        eprintln!("  Nix / NixOS    use the packaged build, which wraps the binary with the");
        eprintln!("                 right loader path:   nix run <flake> -- deck.pdf");
        eprintln!("                 for development:     nix develop   (or nix-shell)");
        eprintln!("  Debian/Ubuntu  sudo apt install libxcursor1 libxkbcommon0 libvulkan1 \\");
        eprintln!("                                  mesa-vulkan-drivers");
        eprintln!("  Fedora         sudo dnf install libXcursor libxkbcommon vulkan-loader");
        eprintln!("  Arch           sudo pacman -S libxcursor libxkbcommon vulkan-icd-loader");
        eprintln!("\nOr point the loader at them yourself:");
        eprintln!("  LD_LIBRARY_PATH=/path/to/libs pulpit deck.pdf");
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!(
            "\nNeither DISPLAY nor WAYLAND_DISPLAY is set: there is no graphical\n\
             session to connect to."
        );
    }
    eprintln!("\nUnderlying error: {text}");
}

fn print_help() {
    println!(
        "pulpit [OPTIONS] [FILE.pdf]\n\
         \n\
         Options:\n\
           --layouts         open the layout library\n\
           --edit-layout ID  open a layout in the designer\n\
           --no-restore      start without restoring an interrupted session\n\
           --render-worker   run as a renderer worker process (internal)\n\
           --document-worker=FILE\n\
                             run as a document worker process (internal)\n\
           -h, --help        show this help\n\
           -V, --version     show the version\n\
         \n\
         Environment:\n\
           PULPIT_PDFIUM_PATH   directory or file holding libpdfium\n\
           PULPIT_CONFIG_DIR    settings directory\n\
           PULPIT_LOG           tracing filter, e.g. debug\n\
           PULPIT_LOG_DIR       persistent log directory"
    );
}

fn run_worker() {
    use pulpit_render::pdf::fixture::FixtureBackend;
    use pulpit_render::pdf::PdfBackend;

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PULPIT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // PDFium is a hard requirement, installed by every supported package. A
    // worker that cannot bind it exits with guidance rather than falling back
    // to placeholder pages, which would show the audience something that is
    // not the deck. Only an explicit request gets the fixture backend.
    let backend: Box<dyn PdfBackend> = if std::env::var_os("PULPIT_FORCE_FIXTURE_BACKEND").is_some()
    {
        Box::new(FixtureBackend::new())
    } else {
        #[cfg(feature = "pdfium")]
        {
            match pulpit_render::pdf::pdfium::PdfiumBackend::bind() {
                Ok(backend) => Box::new(backend),
                Err(e) => {
                    eprintln!(
                        "{}",
                        pulpit_render::pdf::missing_pdfium_message(&e.to_string())
                    );
                    std::process::exit(1);
                }
            }
        }
        #[cfg(not(feature = "pdfium"))]
        {
            eprintln!(
                "{}",
                pulpit_render::pdf::missing_pdfium_message(
                    "this build was compiled without the pdfium feature"
                )
            );
            std::process::exit(1);
        }
    };

    if let Err(e) = pulpit_render::worker::run(std::io::stdin(), std::io::stdout(), backend) {
        tracing::error!(error = %e, "renderer worker exiting");
        std::process::exit(1);
    }
}

/// Serve the document-worker role: open one PDF and answer for it until the
/// supervisor closes the pipe.
///
/// The engine is built here rather than in `pulpit-render` because this is the
/// one place that knows a build may have no PDFium — and a document worker
/// with no PDF library is not a worker that degrades, it is one that has
/// nothing to say. It exits with the same guidance the renderer worker gives.
fn run_document_worker(source: PathBuf) {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PULPIT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    #[cfg(feature = "pdfium")]
    {
        use pulpit_render::document::pdfium::PdfiumDocument;
        use pulpit_render::document::worker::DocumentWorker;
        use pulpit_render::document::PdfDocument;

        let mut backend = match pulpit_render::pdf::pdfium::PdfiumBackend::bind() {
            Ok(backend) => backend,
            Err(e) => {
                eprintln!(
                    "{}",
                    pulpit_render::pdf::missing_pdfium_message(&e.to_string())
                );
                std::process::exit(1);
            }
        };

        // The identities this worker writes into `/NM` have to differ from
        // every other session's, or two people editing two copies of one file
        // would produce annotations that collide when the copies are merged
        // by hand (A3). The process id and the start time are what this
        // process has that no other does; the domain crate reads no clock, so
        // the mixing happens here.
        let seed = seed_from_process();

        let engine = match PdfiumDocument::open(&mut backend, &source) {
            Ok(engine) => engine,
            Err(error) => {
                eprintln!("cannot open {}: {error}", source.display());
                std::process::exit(1);
            }
        };
        let mut worker = DocumentWorker::new();
        worker.adopt(PdfDocument::new(Box::new(engine), seed));

        if let Err(e) = pulpit_render::document::session::serve_stdio(
            worker,
            std::io::stdin(),
            std::io::stdout(),
        ) {
            tracing::error!(error = %e, "document worker exiting");
            std::process::exit(1);
        }
    }
    #[cfg(not(feature = "pdfium"))]
    {
        let _ = source;
        eprintln!(
            "{}",
            pulpit_render::pdf::missing_pdfium_message(
                "this build was compiled without the pdfium feature"
            )
        );
        std::process::exit(1);
    }
}

/// Something this process has that no other does, for annotation identity.
#[cfg(feature = "pdfium")]
fn seed_from_process() -> u64 {
    let pid = u64::from(std::process::id());
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    pid.rotate_left(32) ^ since_epoch
}
