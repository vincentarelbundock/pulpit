//! pulpit: A Snappy and Snazzy PDF Projector.
//!
//! One executable, two roles. Run normally it is the presenter application;
//! run with `--render-worker` it is a renderer worker process, which is how
//! the supervisor spawns its pool without needing a second installed binary.

mod annotation_export;
mod app;
mod designer;
mod designer_view;
mod display;
mod doc;
mod latency;
mod layout;
mod layout_renderer;
mod media;
mod panel;
mod platform;
mod reader;
mod residency;
mod session;
mod settings;
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
    let mut arguments = std::env::args().skip(1);
    let mut document: Option<PathBuf> = None;
    let mut worker = false;
    let mut start_page = StartPage::Presenter;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--render-worker" => worker = true,
            "--typst-worker" => {
                crate::typst_annotation::run_worker();
                return Ok(());
            }
            // A media worker is another role of this same executable, so the
            // supervisor spawns one without a second installed binary.
            _ if argument.starts_with("--media-worker=") => {
                let role = argument.trim_start_matches("--media-worker=");
                if !pulpit_media::worker::run_role(role) {
                    eprintln!("unknown media worker role: {role}");
                    std::process::exit(2);
                }
                return Ok(());
            }
            "--layouts" => start_page = StartPage::Library,
            "--edit-layout" => {
                start_page = match arguments.next() {
                    Some(id) => StartPage::Editor(id),
                    None => {
                        eprintln!("--edit-layout needs a layout id");
                        std::process::exit(2);
                    }
                }
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--version" | "-V" => {
                println!("pulpit {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => document = Some(PathBuf::from(other)),
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
            eprintln!(
                "pulpit is already running (process {pid}).\n\
                 Switch to that window instead — a second copy would open a second\n\
                 audience window and the two would flicker against each other.\n\
                 If that process is gone, delete {}.",
                lock.display()
            );
            tracing::warn!(pid, "refused to start a second instance");
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

    let result = iced::daemon(
        move || app::App::new(document.clone(), start_page.clone(), settings.clone()),
        app::App::update,
        view::view,
    )
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
           --render-worker   run as a renderer worker process (internal)\n\
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
