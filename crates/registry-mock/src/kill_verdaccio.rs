use pipe_trait::Pipe;
use sysinfo::{
    Pid, Process, ProcessExt, ProcessRefreshKind, RefreshKind, Signal, System, SystemExt,
};

pub fn kill_verdaccio_recursive_by_process(process: &Process, signal: Signal) -> u64 {
    let kill_count = process
        .tasks
        .values()
        .map(|process| kill_verdaccio_recursive_by_process(process, signal))
        .sum();
    if process.name().to_lowercase().contains("verdaccio")
        && process.kill_with(signal).unwrap_or_else(|| process.kill())
    {
        kill_count + 1
    } else {
        kill_count
    }
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
