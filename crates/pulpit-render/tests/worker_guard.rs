//! A worker never spawns workers.
//!
//! This is its own test binary on purpose: it sets a process-global variable,
//! and tests inside one binary share a process. The regression it guards is
//! not subtle in effect — a supervisor started from inside a worker spawns a
//! pool, each of whose members spawns a pool, until the machine is gone — so
//! the guard has to hold before any deadline or restart budget is consulted.

use std::time::Duration;

use pulpit_render::supervisor::{
    RendererSupervisor, SupervisorConfig, WorkerCommand, WORKER_MARKER,
};

const WORKER: &str = env!("CARGO_BIN_EXE_pulpit-render-worker");

#[test]
fn a_worker_process_refuses_to_spawn_workers() {
    std::env::set_var("PULPIT_FORCE_FIXTURE_BACKEND", "1");
    let config = |command| SupervisorConfig {
        workers: 2,
        command,
        deadline: Duration::from_secs(5),
        max_restarts: 3,
        restart_window: Duration::from_secs(60),
        retire_after: Duration::from_secs(120),
    };

    // Outside a worker, both forms start normally.
    let explicit = WorkerCommand::Explicit {
        program: WORKER.into(),
        args: Vec::new(),
    };
    assert!(RendererSupervisor::start(config(explicit.clone())).is_ok());

    // Inside one, neither does — including `Explicit`, which recurses just as
    // happily when the program it names is the one already running.
    std::env::set_var(WORKER_MARKER, "1");
    assert!(RendererSupervisor::start(config(explicit)).is_err());
    assert!(RendererSupervisor::start(config(WorkerCommand::CurrentExe {
        arg: "--render-worker".into(),
    }))
    .is_err());
    std::env::remove_var(WORKER_MARKER);
}
