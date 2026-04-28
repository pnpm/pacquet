# Batched I/O Strategies in Pacquet — Report

This report studies four implementations of "write 1000 small files
concurrently" published by KSXGitHub at
[gist 8508bc170bd14365350945bf2c13b800](https://gist.github.com/KSXGitHub/8508bc170bd14365350945bf2c13b800),
compares them against each other on the host this report was generated
on, then maps the lessons onto pacquet's hot install path. The
artifacts are:

| Source file                  | Language | Strategy                                                          |
| ---------------------------- | -------- | ----------------------------------------------------------------- |
| `rayon_and_standard_api.rs`  | Rust     | `rayon::par_iter` + `std::fs::write` (blocking syscalls per file) |
| `tokio_uring_correct.rs`     | Rust     | `tokio_uring`, ring depth 1024, `buffer_unordered(num_cpus*2)`    |
| `tokio_uring_stupid.rs`      | Rust     | `tokio_uring` default ring, unbounded `join_all`, per-file `sync_all` |
| `node_version.js`            | Node     | `Promise.all` over `fs.promises.writeFile`                        |

Each program creates 1000 tiny files (`Data for file <i>`, ~16 bytes
each) under the current working directory.

## 1. Why "stupid" is stupid

The two tokio-uring programs differ in three substantive ways:

```diff
- tokio_uring::start(async {                       // default ring (256 entries)
+ tokio_uring::builder().entries(1024).start(async {
- let mut tasks = Vec::with_capacity(1000);
- for i in 0..1000 {
-     tasks.push(async move { ... write_at + sync_all ... });
- }
- join_all(tasks).await;
+ futures::stream::iter(0..1000)
+     .map(|i| async move { ... write_at only ... })
+     .buffer_unordered(num_cpus::get() * 2)       // 16-32 in flight
+     .collect::<Vec<()>>().await;
```

Three independent failures stack on the "stupid" version:

1. **Submission-queue overflow.** The default ring has 256 entries.
   Pushing 1000 futures in via `join_all` saturates the SQ; subsequent
   submissions block waiting for completions, turning the supposedly
   asynchronous loop into a half-synchronous drip-feed. The "correct"
   version sizes the ring deeper *and* caps in-flight work at
   `num_cpus * 2`, so the SQ never overflows and the runtime keeps
   exactly the right amount of work pipelined to the kernel.

2. **`sync_all` per file.** `sync_all` issues an `fsync(2)` (`IORING_OP_FSYNC`)
   per file, forcing the kernel to flush dirty pages and inode metadata
   to disk before completion fires. For tiny ephemeral files the cost
   is dominated by the journal commit, not the `write` itself —
   amplifying every other inefficiency. The "correct" version skips
   it, matching the `rayon` and `node` baselines (which also don't
   fsync).

3. **No back-pressure.** `join_all` over 1000 futures hands the
   runtime 1000 root tasks to schedule simultaneously. Each polls the
   uring driver, and the driver has to demux completions across all
   1000. `buffer_unordered(N)` keeps exactly `N` futures alive at any
   moment, so the cost of polling and demuxing is bounded.

## 2. Measured numbers (host this run)

Run with `hyperfine --warmup 3 --min-runs 20 --cleanup 'rm -f *.txt'`.
Linux 6.18.5, x86_64, multi-core:

| Strategy                | Mean        | vs. winner    |
| ----------------------- | ----------- | ------------- |
| `rayon` + `std::fs`     | **8.9 ms**  | 1.00×         |
| `tokio_uring` correct   | 49.0 ms     | 5.51× slower  |
| `tokio_uring` stupid    | 133.8 ms    | 15.05× slower |
| Node `Promise.all`      | 152.5 ms    | 17.15× slower |

Two findings worth pinning:

* The "correct" tokio-uring is ~2.7× faster than the "stupid" one,
  confirming the gist's claim. The win comes from ring depth +
  bounded fan-out + skipping fsync, not from io_uring per se.
* **Rayon + `std::fs` beat both io_uring variants by a wide margin
  on this workload.** That deserves explanation rather than being
  treated as a curiosity.

### Why rayon wins on this workload

Three properties of "1000 files × 16 bytes" make io_uring's strengths
irrelevant:

1. **Files fit in the page cache.** Linux acks the `write(2)` as soon
   as bytes land in dirty page cache; there is no real I/O in the
   measurement window. io_uring's whole pitch — fewer context
   switches per I/O — buys nothing when the per-syscall cost is
   already a few hundred nanoseconds.
2. **`open(O_CREAT)` + `close` dominate.** A 16-byte file is one
   write; dirent creation + inode allocation + close is what's
   measured. io_uring does have `IORING_OP_OPENAT2` and `IORING_OP_CLOSE`,
   but they still go through the same VFS path-walk and EXT4 inode
   bitmap as `open(2)`. There is no batching shortcut at the
   filesystem level for 1000 path-distinct files.
3. **`std::fs::write` from a rayon thread is just three syscalls
   (`open`, `write`, `close`) with no intermediate marshalling.**
   Rayon's work-stealing pool spreads these across all cores. The
   io_uring variant pays additional per-op cost: copy `Vec<u8>` into
   the ring buffer, push SQE, kernel reads SQE, kernel pushes CQE,
   user reads CQE, futures driver wakes the right task.

io_uring shines when the per-op latency dominates (network sockets,
random reads from cold storage, large sequential I/O that benefits
from kernel-side batching). For "many tiny synchronous-feeling writes
into the page cache" it is the wrong tool.

## 3. Mapping onto pacquet

Pacquet's installer fans out network and CPU work across a tokio
runtime, then drops to `tokio::task::spawn_blocking` for the CPU /
filesystem-heavy steps. The hot loop with the highest density of
small-file writes is `extract_tarball_entries` in
[`crates/tarball/src/lib.rs`](crates/tarball/src/lib.rs):

```rust
// One spawn_blocking per tarball, gated at num_cpus * 2:
for entry in entries {
    // read tar entry into buffer (CPU)
    // sha512 the buffer            (CPU)
    let (file_path, file_hash) =
        store_dir.write_cas_file(&buffer, file_is_executable)?;  // syscall ladder
    // ...record cas_paths + pkg_files_idx
}
```

`write_cas_file` does (`crates/store-dir/src/cas_file.rs`,
`crates/fs/src/ensure_file.rs`):

* `create_dir_all` for the shard, **cached** so it fires at most once
  per shard byte (`StoreDir::ensured_shards` `DashSet<u8>` —
  `crates/store-dir/src/store_dir.rs:35`),
* `OpenOptions::create_new(true)` + `write_all` + drop (close).

There is no `fsync`, no per-file `create_dir_all` on the cached path,
no atomic rename on the new-file path. This is already close to the
gist's "rayon+std" baseline shape — the gold-medal strategy.

What the design does *not* do is parallelize within a single tarball.
A package with 100 files writes them sequentially on one
blocking-pool thread. Concurrency comes from running ~`num_cpus * 2`
tarballs in parallel.

### Could io_uring help?

Three plausible places to reach for io_uring:

**A. Within-tarball fan-out for CAS writes.** Replace the per-tarball
sequential write loop with a tokio-uring submit-all-and-collect.
Given the gist result (rayon+std beat tokio-uring 5.5× on the same
workload shape), and given that a tarball's worth of CAS files is
typically 10–100 (smaller than the gist's 1000-file workload), the
expected win is negative. The right alternative — if any — is a
within-tarball `rayon::par_iter`, *not* tokio-uring.

**B. Across-tarball CAS writes.** Already parallel via
`spawn_blocking` × `num_cpus * 2`. Adding an io_uring shim on top
would add a layer (driver, SQ/CQ marshaling) without changing the
bound — the bound is `num_cpus * 2`, set deliberately to avoid
oversubscribing CPU during SHA-512.

**C. Bulk shard mkdir at install start.** `StoreDir::init` already
materializes all 256 shards in one tight loop (`store_dir.rs:163`).
This runs once per fresh-store install. io_uring's `IORING_OP_MKDIRAT`
could submit them all in one ring batch, but the existing 256
sequential mkdirs take well under a millisecond on any sane
filesystem; this is not a real bottleneck.

The only candidate where io_uring might plausibly help — and only
plausibly — is the **integrity-check stat fan-out** in
`prefetch_cas_paths` (`crates/tarball/src/lib.rs:516–525`), which
stats every referenced CAS file across ~1000 packages on a warm
install. Cold-cache stats actually do hit the disk and would benefit
from kernel-side batching. This is *not* what the gist's batched-write
benchmark targets, though — it's a different operation (`statx`),
and a separate investigation.

## 4. Proposed micro-benchmark

A new criterion bench was added at
`tasks/micro-benchmark/src/cafs_write.rs`:

* Decompresses a real npm tarball (`@fastify+error`, 14 files,
  payload sizes 28 B – 4.3 KB — representative of npm content).
* Replicates the file list under `replica_NNNN/` prefixes to reach
  workloads of 128 files (one mid-sized package) and 1024 files
  (~one full install fan-out).
* Pre-creates parent directories (matching `StoreDir::init`'s
  pre-shard step), so the measurement is open/write/close + SHA-512
  per file.
* Five strategies:
  * `sequential` — current pacquet within-tarball shape.
  * `rayon` — `par_iter` over the same `ensure_file`.
  * `tokio_blocking_per_file` — std::fs analogue of the gist's
    "stupid" pattern, lots of `spawn_blocking` and unbounded
    `join_all`.
  * `tokio_uring_correct` — gist-style ring=1024 +
    `buffer_unordered(num_cpus*2)`.
  * `tokio_uring_stupid` — gist-style default ring, unbounded
    `join_all`, per-file `sync_all`.

Run via:

```sh
just integrated-benchmark   # for cross-revision integration tests
# or directly:
cargo run --release -p pacquet-micro-benchmark
```

## 5. Measured numbers (pacquet micro-benchmark)

Same host (Linux 6.18.5, x86_64). `cargo run --release -p
pacquet-micro-benchmark`, criterion default settings (3 s warmup, 100
samples per group). The two workloads replicate the
`@fastify+error-3.3.0.tgz` fixture's 14 files until the row count is
hit, with parent dirs pre-created so the measurement is
open + write + close + SHA-512 per file.

### `cafs_write_128` (128 files, ~144 KiB total)

| Strategy                  | Mean       | Throughput       | vs. winner  |
| ------------------------- | ---------- | ---------------- | ----------- |
| **`rayon`**               | **5.03 ms**| **28.69 MiB/s**  | 1.00×       |
| `sequential`              | 5.86 ms    | 24.62 MiB/s      | 1.16× slower|
| `tokio_blocking_per_file` | 10.06 ms   | 14.35 MiB/s      | 2.00× slower|
| `tokio_uring_correct`     | 15.02 ms   | 9.61 MiB/s       | 2.99× slower|
| `tokio_uring_stupid`      | 24.30 ms   | 5.94 MiB/s       | 4.83× slower|

### `cafs_write_1024` (1024 files, ~1.13 MiB total)

| Strategy                  | Mean        | Throughput       | vs. winner  |
| ------------------------- | ----------- | ---------------- | ----------- |
| **`rayon`**               | **120.03 ms**| **9.69 MiB/s**  | 1.00×       |
| `tokio_blocking_per_file` | 193.07 ms   | 6.03 MiB/s       | 1.61× slower|
| `tokio_uring_correct`     | 198.14 ms   | 5.87 MiB/s       | 1.65× slower|
| `tokio_uring_stupid`      | 218.01 ms   | 5.34 MiB/s       | 1.82× slower|
| `sequential`              | 341.00 ms   | 3.41 MiB/s       | 2.84× slower|

The pacquet micro-bench numbers tell the same story as the gist
hyperfine numbers, with one extra interesting datum:

* **Rayon wins at every workload size.** The lead grows with workload:
  1.16× over sequential at 128 files becomes 2.84× at 1024 files,
  because rayon's work-stealing pool turns every additional file
  into another opportunity to amortize scheduling cost across cores.
* **`sequential` is *not* always the worst.** At 128 files it beats
  every async / uring variant — the per-call overhead of crossing
  into the tokio runtime, the blocking pool, or the io_uring driver
  swamps the actual write cost when there are too few writes to
  amortize the setup. Only at 1024 files does sequential drop to the
  bottom because the missing parallelism finally costs more than the
  syscall-batching overhead of the alternatives.
* **The "correct" vs. "stupid" tokio-uring gap shrinks with workload
  size**, from 1.62× at 128 files to 1.10× at 1024 files. The
  unbounded `join_all` shape pays a fixed-cost per task to spin up;
  spread that across 1024 ops and the per-op cost dominates again.
  But "correct" still wins, just by less, and **both still lose to
  rayon by a meaningful margin** at every size.

### Confirming the gist's diagnoses

The pacquet bench reproduces both of the gist's findings, in pacquet's
own code:

* **Ring depth + concurrency cap matter**: `tokio_uring_correct` is
  faster than `tokio_uring_stupid` at both 128 and 1024 files.
* **Per-file `sync_all` is expensive**: the "stupid" variant carries
  it; the "correct" one doesn't. At 128 files the cost is dominant
  (1.62× gap); at 1024 it's still measurable (1.10×).

But the bigger result is that **the right comparison for pacquet's
hot path is not "stupid uring vs. correct uring"** — both are bad
relative to the rayon baseline pacquet's existing shape already
matches.

## 6. Recommendation

**Do not adopt tokio-uring in pacquet.** The current shape — sequential
per-tarball writes, `num_cpus * 2` tarballs in parallel via
`spawn_blocking` — is structurally equivalent to the rayon+std::fs
strategy that won at every workload size measured. Any io_uring layer
on top would add complexity (Linux-only; single-threaded ring driver
per task; lifetime / `'static` buffer constraints; harder error
mapping) for **a measured 1.65×–4.83× regression** on the workload
the change would target.

**Within-tarball rayon parallelism is a credible follow-up.** The 1024
data point shows rayon at **2.84×** sequential. Pacquet today writes
each tarball's CAS files sequentially within one `spawn_blocking`
body; switching that loop to `par_iter` while keeping the
`num_cpus * 2` outer cap could shave a meaningful chunk off install
time on packages with many small files (e.g. babel/eslint plugin
forests). It would *not* change pacquet's overall threading model
— just take more advantage of the threads already available. This
is left as a follow-up; it isn't an io_uring change and isn't
strictly the question this report set out to answer.

**The gist's "stupid" pattern is a useful negative example to keep
in code review.** Two anti-patterns to flag:

1. `join_all` over a large unbounded set of I/O futures without a
   `buffer_unordered` cap.
2. Per-op `fsync` / `sync_all` in a hot batch unless the durability
   guarantee is actually required.

Both already appear in pacquet's review checklists implicitly via the
`post_download_semaphore()` cap and the `ensure_file` design — this
report just makes the rationale explicit and gives them
quantitative weight.

### Best implementation, in one sentence

For pacquet's CAS-write hot path, **rayon `par_iter` over
`std::fs::write` (the gist's "rayon and standard API" baseline) is
the best implementation**, and pacquet's current
`spawn_blocking`-per-tarball shape is the multi-tarball generalization
of it. Neither tokio-uring variant from the gist beats it on this
workload at any size we measured.
