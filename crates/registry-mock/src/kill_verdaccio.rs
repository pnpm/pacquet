use pipe_trait::Pipe;
use sysinfo::{
    Pid, Process, ProcessExt, ProcessRefreshKind, RefreshKind, Signal, System, SystemExt,
};

pub fn kill_verdaccio_recursive_by_process(process: &Process, signal: Signal) -> u64 {
    let kill = |process: &Process| -> u64 {
        if !process.name().to_lowercase().contains("verdaccio") {
            kill_verdaccio_recursive_by_process(process, signal)
        } else if process.kill_with(signal).unwrap_or_else(|| process.kill()) {
            1
        } else {
            0
        }
    };
    process.tasks.values().map(kill).sum()
}

pub fn kill_verdaccio_recursive_by_pid_in(system: &System, pid: Pid, signal: Signal) -> u64 {
    system
        .processes()
        .get(&pid)
        .map_or(0, |process| kill_verdaccio_recursive_by_process(process, signal))
}

pub fn kill_verdaccio_recursive_by_pid(pid: Pid, signal: Signal) -> u64 {
    let system = RefreshKind::new()
        .with_processes(ProcessRefreshKind::new())
        .pipe(System::new_with_specifics);
    kill_verdaccio_recursive_by_pid_in(&system, pid, signal)
}
