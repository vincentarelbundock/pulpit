//! pulpit: A Snappy and Snazzy PDF Projector.
//!
//! One executable, several roles. Run normally it is the presenter
//! application; run with `--render-worker` it is a renderer worker process,
//! and with `--document-worker=FILE` it is a document worker holding one open
//! PDF — or one open folder of images (`SPEC-images.md` §48), which is the
//! same role over a source that answers `Unsupported` to every PDF semantic.
//! Every role is this same binary re-executed with a flag, which is how
//! a supervisor spawns what it needs without a second installed binary to
//! ship, sign or find on `PATH`.

mod app;
mod autocrop;
mod coalesce;
mod datefield;
mod designer;
mod designer_view;
mod disclosure;
mod display;
mod doc;
mod form_flow;
mod keyladder;
mod latency;
mod layout;
mod layout_renderer;
mod media;
mod page_colors;
mod panel;
mod platform;
mod printing;
mod probegen;
mod reader;
mod reader_journal;
mod reader_link;
mod residency;
mod session;
mod settings;
mod signature_profiles;
mod signing;
mod speech;
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

    // The zero the startup marks measure from.
    let _ = STARTED.get_or_init(std::time::Instant::now);

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
    startup_mark("settings loaded");

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
        "pulpit [OPTIONS] [FILE.pdf | IMAGE | DIRECTORY | COMIC.cbz]\n\
         \n\
         A directory is presented as a document whose pages are the images\n\
         directly inside it, in natural name order. Naming one image opens\n\
         the directory it is in, starting on that image. A .cbz or .cbt comic\n\
         archive is read the same way, without being unpacked to disk.\n\
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
           PULPIT_LOG_DIR       persistent log directory\n\
           PULPIT_SPEECH_DIR    read-only directory of speech voices and\n\
                                engines, searched before the user's own"
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

    // Two backends in one worker (SPEC-images.md §45): a directory source is
    // decoded here by the `image` crate, a file source goes to PDFium. The
    // choice is made per open, not once at startup, because a worker holds
    // several documents at a time — always two during a reload — and after
    // this they need not be the same kind.
    //
    // PDFium is still a hard requirement *for a PDF*, installed by every
    // supported package: a worker asked to open a deck it cannot render exits
    // with guidance rather than falling back to placeholder pages, which
    // would show the audience something that is not the deck. Only an
    // explicit request gets the fixture backend.
    let backend: Box<dyn PdfBackend> = Box::new(pulpit_render::pdf::router::RoutingBackend::new(
        Box::new(|| {
            if std::env::var_os("PULPIT_FORCE_FIXTURE_BACKEND").is_some() {
                return Ok(Box::new(FixtureBackend::new()) as Box<dyn PdfBackend>);
            }
            #[cfg(feature = "pdfium")]
            {
                match pulpit_render::pdf::pdfium::PdfiumBackend::bind() {
                    Ok(backend) => Ok(Box::new(backend) as Box<dyn PdfBackend>),
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
        }),
    ));

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

    // A folder of images or a comic archive needs no PDF library at all, and
    // must not be refused because one is missing (SPEC-images.md §45.3,
    // SPEC-reader-formats.md §56.1). Checked before the binding below, which
    // is the whole point of the ordering.
    //
    // A format pulpit refuses outright takes the same door, for the same
    // reason: naming it costs nothing, so a machine without PDFium must still
    // be told its `.epub` is an EPUB rather than that PDFium is missing
    // (§61.4, §65.2).
    if pulpit_render::unsupported_format(&source).is_some()
        || pulpit_render::images::resolve_source(&source).is_some()
    {
        run_image_document_worker(source);
        return;
    }

    // A DjVu needs djvulibre and not PDFium, and the same ordering argument
    // applies in both directions: a missing PDF library must not refuse a
    // scanned book, and a missing DjVu library must not refuse a deck
    // (`SPEC-reader-formats.md` §56.1, §65.2).
    if pulpit_render::is_djvu(&source) {
        run_djvu_document_worker(source);
        return;
    }

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

/// Serve the document-worker role for an image directory (`SPEC-images.md`
/// §48).
///
/// The same loop and the same protocol; only the engine differs, and this one
/// refuses every PDF semantic rather than pretending to have one.
fn run_image_document_worker(source: PathBuf) {
    use pulpit_render::document::worker::DocumentWorker;
    use pulpit_render::document::PdfDocument;
    use pulpit_render::images::ImageDocument;

    let engine = match ImageDocument::open(&source) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("cannot open {}: {error}", source.display());
            std::process::exit(1);
        }
    };
    let mut worker = DocumentWorker::new();
    // Nothing here writes an annotation, so the seed identifies nothing —
    // but the document type is shared and wants one, and a per-process value
    // is what every other engine gets.
    worker.adopt(PdfDocument::new(Box::new(engine), seed_from_process()));

    if let Err(e) =
        pulpit_render::document::session::serve_stdio(worker, std::io::stdin(), std::io::stdout())
    {
        tracing::error!(error = %e, "image document worker exiting");
        std::process::exit(1);
    }
}

/// Serve the document-worker role for a DjVu file (`SPEC-reader-formats.md`
/// §60).
///
/// The same loop and the same protocol again; the engine turns pages and
/// renders them and refuses every PDF semantic. A machine with no djvulibre
/// exits here with the message that names the format and says what would
/// install it — never with a complaint about a damaged file (§61.1, §61.2).
#[cfg(feature = "djvu")]
fn run_djvu_document_worker(source: PathBuf) {
    use pulpit_render::document::worker::DocumentWorker;
    use pulpit_render::document::PdfDocument;
    use pulpit_render::DjvuDocument;

    let engine = match DjvuDocument::open(&source) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let mut worker = DocumentWorker::new();
    // As with the image engine: nothing here writes an annotation, so the
    // seed identifies nothing, but every engine gets one.
    worker.adopt(PdfDocument::new(Box::new(engine), seed_from_process()));

    if let Err(e) =
        pulpit_render::document::session::serve_stdio(worker, std::io::stdin(), std::io::stdout())
    {
        tracing::error!(error = %e, "DjVu document worker exiting");
        std::process::exit(1);
    }
}

/// A build compiled without the DjVu backend still recognises the format and
/// says what is missing, rather than handing the file to PDFium and reporting
/// a scanned book as a damaged PDF (§61.1, §61.2).
#[cfg(not(feature = "djvu"))]
fn run_djvu_document_worker(source: PathBuf) {
    let _ = source;
    // Its own sentence, not `missing_djvu_message`: that one tells the reader
    // to install djvulibre, and on a build with no DjVu backend in it
    // installing djvulibre would change nothing.
    eprintln!("This build of pulpit was compiled without DjVu support.");
    std::process::exit(1);
}

/// Something this process has that no other does, for annotation identity.
fn seed_from_process() -> u64 {
    let pid = u64::from(std::process::id());
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    pid.rotate_left(32) ^ since_epoch
}

/// When the process entered `main`, for the startup timing marks.
///
/// A static rather than a value threaded through `App::new`, because the
/// interesting spans cross the iced boundary: process entry to `App::new`
/// returning, to the presenter window opening, to the deferred probes
/// starting. `tracing` timestamps say the same thing less legibly; one
/// number relative to entry is what a launch regression is diagnosed with.
static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Log a startup stage with its offset from process entry.
pub(crate) fn startup_mark(stage: &str) {
    let started = *STARTED.get_or_init(std::time::Instant::now);
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        stage,
        "startup"
    );
}
