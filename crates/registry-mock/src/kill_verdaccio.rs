use pipe_trait::Pipe;
use sysinfo::{
    Pid, Process, ProcessExt, ProcessRefreshKind, RefreshKind, Signal, System, SystemExt,
};

fn is_descent_of(process: &Process, suspect_ancestor: Pid, system: &System) -> bool {
    let Some(parent) = process.parent() else { return false };
    if parent == suspect_ancestor {
        return true;
    }
    let Some(parent) = system.processes().get(&parent) else { return false };
    is_descent_of(parent, suspect_ancestor, system)
}

pub fn kill_all_verdaccio_children_in(root: Pid, signal: Signal, system: &System) -> usize {
    system
        .processes()
        .values()
        .filter(|process| is_descent_of(process, root, system))
        .filter(|process| process.kill_with(signal).unwrap_or_else(|| process.kill()))
        .count()
}

pub fn kill_all_verdaccio_children(root: Pid, signal: Signal) -> usize {
    let system = RefreshKind::new()
        .with_processes(ProcessRefreshKind::new())
        .pipe(System::new_with_specifics);
    kill_all_verdaccio_children_in(root, signal, &system)
}
