use crate::registry_mock;
use pipe_trait::Pipe;
use std::{
    env, iter,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use which::which_in;

static NODE_REGISTRY_MOCK: OnceLock<PathBuf> = OnceLock::new();

fn init() -> PathBuf {
    let bin = registry_mock().join("node_modules").join(".bin");
    let paths = env::var_os("PATH")
        .unwrap_or_default()
        .pipe_ref(env::split_paths)
        .chain(iter::once(bin))
        .pipe(env::join_paths)
        .expect("append node_modules/.bin to PATH");
    which_in("registry-mock", Some(paths), ".").expect("find registry-mock binary")
}

pub fn node_registry_mock() -> &'static Path {
    NODE_REGISTRY_MOCK.get_or_init(init)
}
