//! `pulpit-topology` — dump or watch the monitor topology.
//!
//! This is the capture half of the CI loop: run it on the machine with the
//! awkward dock, the projector that renumbers itself, or the VKMS connectors
//! being scripted, and it prints topologies in the same format
//! `crates/pulpit-display/tests/topology/*.txt` uses. Commit the output
//! and the behaviour is pinned forever, with no hardware required afterwards.
//!
//! ```text
//! pulpit-topology                  # print the current topology once
//! pulpit-topology --watch          # append a step on every change
//! pulpit-topology --watch --timeout 60 > dock.txt
//! ```

use std::time::{Duration, Instant};

use pulpit_display::scenario::{format_monitor, Scenario, Step};
use pulpit_display::{DisplayBackend, DisplaySnapshot};

fn main() {
    let mut watch = false;
    let mut timeout = None;
    let mut description = String::from("captured with pulpit-topology");
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--watch" | "-w" => watch = true,
            "--timeout" | "-t" => {
                timeout = arguments
                    .next()
                    .and_then(|value| value.parse().ok())
                    .map(Duration::from_secs)
            }
            "--description" | "-d" => {
                description = arguments.next().unwrap_or(description);
            }
            "--help" | "-h" => {
                eprintln!(
                    "pulpit-topology [--watch] [--timeout SECONDS] [--description TEXT]\n\
                     \n\
                     Prints the monitor topology in the scenario format used by\n\
                     pulpit-display's scripted topology tests."
                );
                return;
            }
            other => {
                eprintln!("unknown argument {other:?}");
                std::process::exit(2);
            }
        }
    }

    let Some(backend) = connect() else {
        eprintln!(
            "no display adapter: this needs an X11 session (DISPLAY) or a Wayland \
             session (WAYLAND_DISPLAY)"
        );
        std::process::exit(1);
    };

    eprintln!("# adapter: {}", backend.name());
    eprintln!("# capabilities: {:?}", backend.capabilities());

    let mut previous: Option<DisplaySnapshot> = None;
    let started = Instant::now();
    let mut step = 0;

    println!("# {description}");
    loop {
        match backend.snapshot() {
            Ok(snapshot) => {
                let changed = previous
                    .as_ref()
                    .map(|old| !old.same_topology(&snapshot))
                    .unwrap_or(true);
                if changed {
                    step += 1;
                    print_step(&snapshot, step);
                    previous = Some(snapshot);
                }
            }
            Err(e) => eprintln!("# enumeration failed: {e}"),
        }

        if !watch {
            return;
        }
        if timeout.is_some_and(|limit| started.elapsed() >= limit) {
            eprintln!("# stopping after {step} topologies");
            return;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn print_step(snapshot: &DisplaySnapshot, step: usize) {
    println!("\nstep capture-{step}");
    for monitor in &snapshot.monitors {
        println!("  {}", format_monitor(monitor));
    }
    // Overlaps and mirror groups are derived, not stored, but printing them
    // as comments makes a captured file readable by a human later.
    for overlap in snapshot.overlaps() {
        println!(
            "  # overlap between {} and {} (nested: {})",
            overlap.a, overlap.b, overlap.nested
        );
    }
    if snapshot.mirror_groups().len() < snapshot.len() {
        println!("  # some outputs are exact mirrors and collapse to one target");
    }
    // Round-trip check: a file that cannot be parsed back is useless.
    let step = Step {
        name: format!("capture-{step}"),
        monitors: snapshot.monitors.clone(),
    };
    let text = Scenario {
        description: String::new(),
        steps: vec![step],
    }
    .to_text();
    if let Err(e) = Scenario::parse(&text) {
        eprintln!("# WARNING: this capture does not parse back: {e}");
    }
}

fn connect() -> Option<Box<dyn DisplayBackend>> {
    // Each arm mirrors the module gating in `lib.rs`: a backend is only named
    // where its module exists.
    #[cfg(target_os = "macos")]
    {
        match pulpit_display::macos::MacosBackend::connect() {
            Ok(backend) => return Some(Box::new(backend)),
            Err(e) => eprintln!("# coregraphics: {e}"),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match pulpit_display::windows::WindowsBackend::connect() {
            Ok(backend) => return Some(Box::new(backend)),
            Err(e) => eprintln!("# win32: {e}"),
        }
    }
    #[cfg(all(feature = "wayland", unix, not(target_os = "macos")))]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            match pulpit_display::wayland::WaylandBackend::connect() {
                Ok(backend) => {
                    for check in backend.scale_checks().unwrap_or_default() {
                        eprintln!("# scale check: {}", check.describe());
                    }
                    return Some(Box::new(backend));
                }
                Err(e) => eprintln!("# wayland: {e}"),
            }
        }
    }
    #[cfg(all(feature = "x11", unix, not(target_os = "macos")))]
    {
        if std::env::var_os("DISPLAY").is_some() {
            match pulpit_display::x11::X11Backend::connect() {
                Ok(backend) => {
                    if let Err(e) = backend.subscribe_to_topology_changes() {
                        eprintln!("# x11: could not subscribe to topology change events: {e}");
                    }
                    return Some(Box::new(backend));
                }
                Err(e) => eprintln!("# x11: {e}"),
            }
        }
    }
    None
}
