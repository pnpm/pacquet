//! Compare batched-write strategies for the CAFS hot path.
//!
//! Pacquet's tarball extractor opens, writes, and closes one CAS file per
//! regular tar entry. The current shape is sequential within a tarball
//! (one `spawn_blocking` body per tarball, gated at `num_cpus * 2`). This
//! benchmark measures how alternative batching strategies — rayon
//! `par_iter`, `tokio::task::spawn_blocking` per file, and `tokio-uring`
//! with and without a concurrency cap — compare on the same workload.
//!
//! See <https://gist.github.com/KSXGitHub/8508bc170bd14365350945bf2c13b800>
//! for the reference "stupid vs. correct" tokio-uring pair this benchmark
//! reproduces inline.

use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use criterion::{BenchmarkId, Criterion, Throughput};
use pacquet_fs::ensure_file;
use sha2::{Digest, Sha512};
use tempfile::tempdir;
use zune_inflate::{DeflateDecoder, DeflateOptions};

/// One pre-resolved (relative path, contents) pair from a real npm tarball.
/// `relative_path` is unique per row, so writing into a fresh temp dir
/// never collides with a previous row.
#[derive(Clone)]
struct Entry {
    relative_path: PathBuf,
    content: Arc<Vec<u8>>,
}

/// Decompress + walk the fixture tarball, collecting every regular-file
/// entry. The resulting list is replicated under unique parent dirs to
/// reach `target_count`, so each iteration writes a workload with
/// realistic file-size distribution but enough rows to expose batching
/// overhead.
fn load_workload(fixture: &Path, target_count: usize) -> Vec<Entry> {
    let bytes = fs::read(fixture).expect("read fixture tarball");
    let inflated = DeflateDecoder::new_with_options(
        &bytes,
        DeflateOptions::default().set_confirm_checksum(false),
    )
    .decode_gzip()
    .expect("inflate fixture");

    let mut archive = tar::Archive::new(Cursor::new(inflated));
    let mut base: Vec<Entry> = Vec::new();
    for entry in archive.entries().expect("read tar entries") {
        let mut entry = entry.expect("tar entry");
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().expect("entry path").into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("read entry body");
        base.push(Entry {
            relative_path: path.components().skip(1).collect::<PathBuf>(),
            content: Arc::new(buf),
        });
    }

    // Replicate under `replica_NN/` prefixes until we hit the target row
    // count. Each replica writes the same bytes to a different path, so
    // every strategy is doing genuine FS work (no `O_EXCL` collisions
    // collapsing the workload into a faster verify path).
    let unit = base.len().max(1);
    let replicas = target_count.div_ceil(unit);
    let mut out = Vec::with_capacity(replicas * unit);
    for replica in 0..replicas {
        for entry in &base {
            out.push(Entry {
                relative_path: PathBuf::from(format!("replica_{replica:04}"))
                    .join(&entry.relative_path),
                content: Arc::clone(&entry.content),
            });
        }
        if out.len() >= target_count {
            break;
        }
    }
    out.truncate(target_count);
    out
}

/// Total payload bytes across the workload — used as the criterion
/// throughput so MB/s is comparable between strategies.
fn total_bytes(workload: &[Entry]) -> u64 {
    workload.iter().map(|e| e.content.len() as u64).sum()
}

/// Pre-create every parent directory under `root`. The CAFS hot path
/// caches "shard already created" so per-file `create_dir_all` doesn't
/// fire; mirroring that here keeps strategies focused on the
/// open/write/close cost rather than directory creation.
fn pre_create_dirs(root: &Path, workload: &[Entry]) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for entry in workload {
        if let Some(parent) = entry.relative_path.parent()
            && seen.insert(parent.to_path_buf())
        {
            fs::create_dir_all(root.join(parent)).expect("pre-create dir");
        }
    }
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Baseline: one thread, sequential `ensure_file`. Mirrors what
/// `extract_tarball_entries` does today inside a single
/// `spawn_blocking` body.
fn run_sequential(root: &Path, workload: &[Entry]) {
    for entry in workload {
        let path = root.join(&entry.relative_path);
        // We compute the SHA inline so the per-entry CPU budget matches
        // what the real CAFS write does. Discard the digest — the
        // benchmark cares about wall time, not the value.
        let _ = Sha512::digest(entry.content.as_slice());
        ensure_file(&path, &entry.content, None).expect("sequential ensure_file");
    }
}

/// Rayon `par_iter`. Distributes writes across the global rayon pool;
/// each worker calls the same `ensure_file` as the sequential path.
fn run_rayon(root: &Path, workload: &[Entry]) {
    use rayon::prelude::*;
    workload.par_iter().for_each(|entry| {
        let path = root.join(&entry.relative_path);
        let _ = Sha512::digest(entry.content.as_slice());
        ensure_file(&path, &entry.content, None).expect("rayon ensure_file");
    });
}

/// "Stupid" tokio variant: one `spawn_blocking` per file under the
/// default 512-thread blocking pool, with `join_all` waiting on the
/// whole batch. Mirrors the unbounded-fanout shape the gist's
/// `tokio_uring_stupid.rs` was warning about, but on `std::fs`.
fn run_tokio_blocking_per_file(rt: &tokio::runtime::Runtime, root: &Path, workload: &[Entry]) {
    use futures_util::future::join_all;
    rt.block_on(async {
        let tasks: Vec<_> = workload
            .iter()
            .map(|entry| {
                let path = root.join(&entry.relative_path);
                let content = Arc::clone(&entry.content);
                tokio::task::spawn_blocking(move || {
                    let _ = Sha512::digest(content.as_slice());
                    ensure_file(&path, &content, None).expect("spawn_blocking ensure_file");
                })
            })
            .collect();
        for join_result in join_all(tasks).await {
            join_result.expect("spawn_blocking join");
        }
    });
}

#[cfg(target_os = "linux")]
mod uring {
    use super::*;
    use futures_util::stream::{self, StreamExt};

    /// "Correct" tokio-uring shape from the gist: deep ring,
    /// `buffer_unordered` cap matching `num_cpus * 2`, no per-file
    /// fsync. The SHA is computed before `submit` so kernel queue
    /// depth measures pure write throughput, not CPU.
    pub(super) fn run_correct(root: &Path, workload: &[Entry]) {
        let concurrency = num_cpus::get().saturating_mul(2).max(4);
        let workload = workload.to_vec();
        let root = root.to_path_buf();

        tokio_uring::builder().entries(1024).start(async move {
            stream::iter(workload)
                .map(|entry| {
                    let path = root.join(&entry.relative_path);
                    async move {
                        let _ = Sha512::digest(entry.content.as_slice());
                        let file =
                            tokio_uring::fs::File::create(&path).await.expect("uring create");
                        let buf: Vec<u8> = (*entry.content).clone();
                        let (res, _) = file.write_at(buf, 0).submit().await;
                        res.expect("uring write_at");
                    }
                })
                .buffer_unordered(concurrency)
                .for_each(|_| async {})
                .await;
        });
    }

    /// "Stupid" shape: default ring depth, unbounded `join_all`,
    /// `sync_all` per file. Reproduced verbatim from the gist so we
    /// can show the SQ overflow / fsync pile-up cost relative to the
    /// "correct" version.
    pub(super) fn run_stupid(root: &Path, workload: &[Entry]) {
        let workload = workload.to_vec();
        let root = root.to_path_buf();

        tokio_uring::start(async move {
            let mut tasks = Vec::with_capacity(workload.len());
            for entry in workload {
                let path = root.join(&entry.relative_path);
                tasks.push(async move {
                    let _ = Sha512::digest(entry.content.as_slice());
                    let file = tokio_uring::fs::File::create(&path).await.expect("uring create");
                    let buf: Vec<u8> = (*entry.content).clone();
                    let (res, _) = file.write_at(buf, 0).submit().await;
                    res.expect("uring write_at");
                    file.sync_all().await.expect("uring sync_all");
                });
            }
            futures_util::future::join_all(tasks).await;
        });
    }
}

// ---------------------------------------------------------------------------
// Bench wiring
// ---------------------------------------------------------------------------

pub fn bench_cafs_write(c: &mut Criterion, fixtures_folder: &Path) {
    let fixture = fixtures_folder.join("@fastify+error-3.3.0.tgz");

    // Two file counts: one matches a single mid-sized package; one
    // approximates a full install fan-out without going so big that
    // the bench takes forever per sample.
    for target_count in [128usize, 1024] {
        let workload = load_workload(&fixture, target_count);
        let bytes = total_bytes(&workload);
        let mut group = c.benchmark_group(format!("cafs_write_{target_count}"));
        group.throughput(Throughput::Bytes(bytes));

        // Sequential baseline.
        group.bench_function(BenchmarkId::new("strategy", "sequential"), |b| {
            b.iter_batched_ref(
                || {
                    let dir = tempdir().expect("tempdir");
                    pre_create_dirs(dir.path(), &workload);
                    dir
                },
                |dir| run_sequential(dir.path(), &workload),
                criterion::BatchSize::PerIteration,
            );
        });

        // Rayon par_iter.
        group.bench_function(BenchmarkId::new("strategy", "rayon"), |b| {
            b.iter_batched_ref(
                || {
                    let dir = tempdir().expect("tempdir");
                    pre_create_dirs(dir.path(), &workload);
                    dir
                },
                |dir| run_rayon(dir.path(), &workload),
                criterion::BatchSize::PerIteration,
            );
        });

        // Tokio spawn_blocking per file (the std::fs analogue of the
        // gist's stupid io_uring).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        group.bench_function(BenchmarkId::new("strategy", "tokio_blocking_per_file"), |b| {
            b.iter_batched_ref(
                || {
                    let dir = tempdir().expect("tempdir");
                    pre_create_dirs(dir.path(), &workload);
                    dir
                },
                |dir| run_tokio_blocking_per_file(&rt, dir.path(), &workload),
                criterion::BatchSize::PerIteration,
            );
        });

        #[cfg(target_os = "linux")]
        {
            group.bench_function(BenchmarkId::new("strategy", "tokio_uring_correct"), |b| {
                b.iter_batched_ref(
                    || {
                        let dir = tempdir().expect("tempdir");
                        pre_create_dirs(dir.path(), &workload);
                        dir
                    },
                    |dir| uring::run_correct(dir.path(), &workload),
                    criterion::BatchSize::PerIteration,
                );
            });

            group.bench_function(BenchmarkId::new("strategy", "tokio_uring_stupid"), |b| {
                b.iter_batched_ref(
                    || {
                        let dir = tempdir().expect("tempdir");
                        pre_create_dirs(dir.path(), &workload);
                        dir
                    },
                    |dir| uring::run_stupid(dir.path(), &workload),
                    criterion::BatchSize::PerIteration,
                );
            });
        }

        group.finish();
    }
}
