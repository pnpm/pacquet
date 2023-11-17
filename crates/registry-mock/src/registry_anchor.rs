use crate::{kill_verdaccio::kill_all_verdaccio_children, MockInstanceOptions, RegistryInfo};
use advisory_lock::{AdvisoryFileLock, FileLockError, FileLockMode};
use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};
use std::{
    env::temp_dir,
    fs::{self, File, OpenOptions},
    mem::forget,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use sysinfo::{Pid, PidExt, Signal};

#[derive(Debug, Deserialize, Serialize)]
pub struct RegistryAnchor {
    pub ref_count: u32,
    pub info: RegistryInfo,
}

impl Drop for RegistryAnchor {
    fn drop(&mut self) {
        // information from self is outdated, do not use it.

        let guard = GuardFile::lock();

        // load an up-to-date anchor, it is leaked to prevent dropping (again).
        let anchor = RegistryAnchor::load().pipe(Box::new).pipe(Box::leak);
        if self.info != anchor.info {
            eprintln!("info: {:?} is outdated. Skip.", &self.info);
            return;
        }

        if let Some(ref_count) = anchor.ref_count.checked_sub(1) {
            anchor.ref_count = ref_count;
            anchor.save();
            if ref_count > 0 {
                eprintln!("info: The mocked server is still used by {ref_count} users. Skip.");
                return;
            }
        }

        let pid = anchor.info.pid;
        eprintln!("info: There are no more users that use the mocked server");
        eprintln!("info: Terminating all verdaccio instances below {pid}...");
        let kill_count = kill_all_verdaccio_children(Pid::from_u32(pid), Signal::Interrupt);
        eprintln!("info: Terminated {kill_count} verdaccio instances");

        RegistryAnchor::delete();
        guard.unlock();
    }
}

impl RegistryAnchor {
    fn path() -> &'static Path {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| temp_dir().join("pacquet-registry-mock-anchor.json"))
    }

    fn load() -> Self {
        RegistryAnchor::path()
            .pipe(fs::read_to_string)
            .expect("read the anchor")
            .pipe_as_ref(serde_json::from_str)
            .expect("parse anchor")
    }

    fn save(&self) {
        let text = serde_json::to_string_pretty(self).expect("convert anchor to JSON");
        fs::write(RegistryAnchor::path(), text).expect("write to anchor");
    }

    pub fn load_or_init(init_options: MockInstanceOptions<'_>) -> Self {
        if let Some(guard) = GuardFile::try_lock() {
            let mock_instance = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime")
                .block_on(init_options.spawn());
            let port = init_options.port;
            let pid = mock_instance.process.id();
            let info = RegistryInfo { port, pid };
            let anchor = RegistryAnchor { ref_count: 1, info };
            anchor.save();
            guard.unlock();
            forget(mock_instance); // prevent this process from killing itself on drop
            anchor
        } else {
            let guard = GuardFile::lock();
            let mut anchor = RegistryAnchor::load();
            anchor.ref_count = anchor.ref_count.checked_add(1).expect("increment ref_count");
            anchor.save();
            guard.unlock();
            anchor
        }
    }

    fn delete() {
        if let Err(error) = fs::remove_file(RegistryAnchor::path()) {
            eprintln!("warn: Failed to delete the anchor file: {error}");
        }
    }
}

#[must_use]
struct GuardFile;

impl Drop for GuardFile {
    fn drop(&mut self) {
        GuardFile::path().unlock().expect("release file guard");
    }
}

impl GuardFile {
    fn path() -> &'static File {
        static PATH: OnceLock<File> = OnceLock::new();
        PATH.get_or_init(|| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(temp_dir().join("pacquet-registry-mock-anchor.lock"))
                .expect("open the guard file")
        })
    }

    fn lock() -> Self {
        GuardFile::path().lock(FileLockMode::Exclusive).expect("acquire file guard");
        GuardFile
    }

    fn try_lock() -> Option<Self> {
        match GuardFile::path().try_lock(FileLockMode::Exclusive) {
            Ok(()) => Some(GuardFile),
            Err(FileLockError::AlreadyLocked) => None,
            Err(FileLockError::Io(error)) => panic!("Failed to acquire the file guard: {error}"),
        }
    }

    fn unlock(self) {
        drop(self)
    }
}
