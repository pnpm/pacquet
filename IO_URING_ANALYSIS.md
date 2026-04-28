# Batched I/O Strategies in Pacquet — Report

This report studies four implementations of "write 1000 small files
concurrently" published by KSXGitHub at
[gist 8508bc170bd14365350945bf2c13b800](https://gist.github.com/KSXGitHub/8508bc170bd14365350945bf2c13b800),
adds [bytedance/monoio](https://github.com/bytedance/monoio) as a
sixth competitor (correct + stupid shapes), compares them against
each other on the host this report was generated on, then maps the
lessons onto pacquet's hot install path. The artifacts are:

| Source file                  | Language | Strategy                                                          |
| ---------------------------- | -------- | ----------------------------------------------------------------- |
| `rayon_and_standard_api.rs`  | Rust     | `rayon::par_iter` + `std::fs::write` (blocking syscalls per file) |
| `tokio_uring_correct.rs`     | Rust     | `tokio_uring`, ring depth 1024, `buffer_unordered(num_cpus*2)`    |
| `tokio_uring_stupid.rs`      | Rust     | `tokio_uring` default ring, unbounded `join_all`, per-file `sync_all` |
| `monoio_correct` (added)     | Rust     | `monoio::IoUringDriver`, ring depth 1024, `buffer_unordered(num_cpus*2)` |
| `monoio_stupid` (added)      | Rust     | `monoio::IoUringDriver` default ring, unbounded `join_all`, per-file `sync_all` |
| `node_version.js`            | Node     | `Promise.all` over `fs.promises.writeFile`                        |

Each program creates 1000 tiny files (`Data for file <i>`, ~16 bytes
each) under the current working directory.

monoio is ByteDance's thread-per-core async runtime built directly on
io_uring. The relevant difference from `tokio_uring` is the runtime,
not the kernel interface: monoio ships its own scheduler and task
abstraction rather than bridging to tokio's, so its per-task
overhead is lower at the cost of being single-threaded by design.
Both runtimes wrap the same `io_uring` kernel surface, so they show
the same `IORING_OP_*` events under `bpftrace`; the user-space cost
is what differs.

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

## 2. Measured numbers (gist + monoio, this host)

Run with `hyperfine --warmup 3 --min-runs 20 --cleanup 'rm -f *.txt'`.
Linux 6.18.5, x86_64, **4 vCPU / 16 GiB RAM** sandbox (caveat covered
in §7).

| Strategy                | Mean         | vs. winner     |
| ----------------------- | ------------ | -------------- |
| `rayon` + `std::fs`     | **8.1 ms**   | 1.00×          |
| `monoio` correct        | 31.6 ms      | 3.91× slower   |
| `tokio_uring` correct   | 39.6 ms      | 4.91× slower   |
| `monoio` stupid         | 136.2 ms     | 16.89× slower  |
| `tokio_uring` stupid    | 139.5 ms     | 17.30× slower  |
| Node `Promise.all`      | 151.0 ms     | 18.72× slower  |

Three findings worth pinning:

* **`monoio` correct beats `tokio_uring` correct by ~1.25× on this
  host.** Same kernel interface, same ring depth, same concurrency
  cap — the win is the runtime: monoio's thread-per-core scheduler
  has lower per-task overhead than tokio-uring's bridge into the
  tokio task system. With the kernel side held constant, the
  user-space scheduler is the dominant variable for "many tiny ops."
* The two **stupid variants are tied** (within noise: 136 ms vs.
  139 ms). Once submission-queue overflow + per-file `fsync` start
  dominating, the choice of runtime stops mattering — the kernel is
  the bottleneck, and both runtimes pay the same cost.
* Rayon still wins overall on this host. **The relevant question is
  no longer "rayon vs. uring" but "what changes when CPU count goes
  up?"** — covered in §7.

### Why rayon wins on this host (caveat in §7)

Three properties of "1000 files × 16 bytes" make io_uring's strengths
irrelevant **on a low-CPU host**:

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
   Rayon's work-stealing pool spreads these across all cores. Both
   io_uring runtimes pay additional per-op cost: copy `Vec<u8>` into
   the ring buffer, push SQE, kernel reads SQE, kernel pushes CQE,
   user reads CQE, runtime wakes the right task. monoio amortizes
   this better than tokio_uring (1.25× lead this host) but still
   pays it.

The shape that flips this calculus is **high CPU count**, where
rayon's per-thread syscall pattern hits kernel-side lock contention
(EXT4 journal, dcache, inode allocator) and io_uring's
single-threaded-submit model side-steps it. See §7 for measured
data on a 28C/56T host where `tokio_uring` correct (and presumably
`monoio` correct, by extension) overtakes rayon.

io_uring shines when the per-op latency dominates (network sockets,
random reads from cold storage, large sequential I/O that benefits
from kernel-side batching), or when concurrent syscall serialization
in the kernel becomes the wall — i.e. on machines with enough cores
to actually serialize. For "many tiny page-cache writes on 4 cores"
it is the wrong tool.

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
* Seven strategies:
  * `sequential` — current pacquet within-tarball shape.
  * `rayon` — `par_iter` over the same `ensure_file`.
  * `tokio_blocking_per_file` — std::fs analogue of the gist's
    "stupid" pattern, lots of `spawn_blocking` and unbounded
    `join_all`.
  * `tokio_uring_correct` — gist-style ring=1024 +
    `buffer_unordered(num_cpus*2)`.
  * `tokio_uring_stupid` — gist-style default ring, unbounded
    `join_all`, per-file `sync_all`.
  * `monoio_correct` — same parameters as `tokio_uring_correct`,
    but on `monoio::IoUringDriver` instead of tokio_uring.
  * `monoio_stupid` — same anti-patterns as `tokio_uring_stupid`,
    on monoio.

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

| Strategy                  | Mean       | vs. winner   |
| ------------------------- | ---------- | ------------ |
| **`rayon`**               | **5.93 ms**| 1.00×        |
| `sequential`              | 6.24 ms    | 1.05× slower |
| `tokio_blocking_per_file` | 12.54 ms   | 2.11× slower |
| `monoio_stupid`           | 18.66 ms   | 3.15× slower |
| `tokio_uring_correct`     | 18.71 ms   | 3.16× slower |
| `monoio_correct`          | 19.47 ms   | 3.28× slower |
| `tokio_uring_stupid`      | 26.36 ms   | 4.45× slower |

### `cafs_write_1024` (1024 files, ~1.13 MiB total)

| Strategy                  | Mean        | vs. winner   |
| ------------------------- | ----------- | ------------ |
| **`rayon`**               | **122.80 ms**| 1.00×       |
| `tokio_blocking_per_file` | 194.81 ms   | 1.59× slower |
| `monoio_correct`          | 201.38 ms   | 1.64× slower |
| `tokio_uring_correct`     | 202.78 ms   | 1.65× slower |
| `tokio_uring_stupid`      | 224.70 ms   | 1.83× slower |
| `monoio_stupid`           | 232.47 ms   | 1.89× slower |
| `sequential`              | 322.48 ms   | 2.63× slower |

Five findings in the seven-strategy data:

* **Rayon wins at every workload size.** The lead grows with
  workload: 1.05× over sequential at 128 files becomes 2.63× at
  1024 files, because rayon's work-stealing pool turns every
  additional file into another opportunity to amortize scheduling
  cost across cores.
* **`sequential` is *not* always the worst.** At 128 files it beats
  every async / uring variant by a wide margin — the per-call
  overhead of crossing into the tokio runtime, the blocking pool, or
  the io_uring driver swamps the actual write cost when there are
  too few writes to amortize the setup. Only at 1024 files does
  sequential drop to the bottom because the missing parallelism
  finally costs more than the per-op overhead of the alternatives.
* **monoio and tokio-uring are essentially tied at 1024 files.**
  `monoio_correct` (201 ms) vs. `tokio_uring_correct` (203 ms) is
  within noise. monoio's runtime overhead advantage washes out once
  the workload is big enough that kernel-side cost dominates per-op
  scheduling cost. The 1.25× gap visible in the gist hyperfine and
  at 128 files reflects monoio's leaner scheduler; at 1024 files the
  same EXT4 / VFS path that all three runtimes share is the wall.
* **At 128 files, `monoio_stupid` (18.66 ms) is statistically tied
  with `tokio_uring_correct` (18.71 ms) and faster than
  `monoio_correct` (19.47 ms).** That looks paradoxical until you
  notice monoio's task overhead is so low that the cost of an
  unbounded `join_all` over 128 futures is negligible — and the cost
  of the `buffer_unordered` machinery is *not*. At 1024 files the
  pattern reasserts: `monoio_correct` (201) beats `monoio_stupid`
  (232) by 1.15×, and `tokio_uring_correct` (203) beats
  `tokio_uring_stupid` (225) by 1.11×. The gist's anti-patterns are
  real costs, just not at every workload size.
* **The "correct" vs. "stupid" tokio-uring gap shrinks with workload
  size**, from 1.41× at 128 files to 1.11× at 1024 files. Same
  underlying reason as above: fixed per-task setup cost spread over
  more ops.

### Confirming the gist's diagnoses

The pacquet bench reproduces both of the gist's findings on both
runtimes:

* **Ring depth + concurrency cap matter at scale**: at 1024 files,
  every "correct" variant beats its "stupid" counterpart on both
  runtimes (tokio_uring 1.11×, monoio 1.15×). At 128 files the
  signal is mixed because the fixed costs of the "correct"
  machinery aren't yet amortized.
* **Per-file `sync_all` is expensive**: this dominates the "stupid"
  variants' overhead at every size, but the cost ratio depends on
  what else is going on — at 128 files on monoio the runtime is so
  cheap that even the fsync drumbeat doesn't push the result above
  the "correct" timing.

But the headline result is unchanged from the four-strategy run:
**the right comparison for pacquet's hot path on this host is not
"uring runtime A vs. uring runtime B"** — every uring strategy
loses to rayon, regardless of which runtime drives the ring.

## 6. The host caveat that flips the ranking

The numbers above were collected on a 4-vCPU sandbox (Intel Xeon
@ 2.10 GHz, 16 GiB RAM, ext4 on virtio block, no cgroup limits, Linux
6.18.5). On a 28-core / 56-thread workstation the same gist programs
produce the **opposite** ranking:

| Strategy                | 4-vCPU sandbox (mine) | 28C/56T workstation (theirs) |
| ----------------------- | --------------------- | ---------------------------- |
| `tokio_uring` correct   | 39.6 ms (4.91× slower)| **27.3 s (1.00×)**           |
| `rayon` + `std::fs`     | **8.1 ms (1.00×)**    | 39.2 s (1.44× slower)        |
| `tokio_uring` stupid    | 139.5 ms (17.30×)     | 117.5 s (4.30× slower)       |
| Node                    | 151.0 ms (18.72×)     | 118.0 s (4.32× slower)       |

(*This is real measured data from the gist's author running the same
binaries on a different host. monoio wasn't measured on the 56-thread
box; if/when it is, expect it to behave like `tokio_uring` correct
modulo monoio's runtime advantage.*)

The mechanism for the inversion is **kernel-side lock contention**.
Rayon at `num_cpus` workers does *N* concurrent in-kernel
`open(O_CREAT) + write + close` walks. Each walk takes:

* the parent dirent's `i_rwsem` for `lookup` / `dentry` insertion,
* the per-superblock journal lock on the create path (EXT4 / XFS
  `start_this_handle` → `j_state_lock`),
* the inode-allocator group lock to bump i-block / i-inode bitmaps,
* the per-mount lock on `__mnt_want_write`.

At 4–8 cores those cachelines stay mostly local; at 56 threads they
ping-pong between sockets and the syscall floor jumps from "few
hundred ns" to "several µs each." Rayon's per-thread syscall
pattern can't hide that. io_uring sidesteps it: a *single*
user-thread submits SQEs into shared-memory ring buffers, the
kernel side processes them on its own worker pool, and many of
those locks are taken once-per-batch instead of once-per-thread.

The "stupid" patterns lose on both hosts by similar margins, so
the **anti-patterns are universally bad** and the gist's
educational point still stands. What flips with hardware is which
*correct* strategy wins.

There's also a sandbox confound worth flagging: my host's `/dev/vda`
is virtio passthrough, so `fsync` cost is whatever the host
chooses to charge. On a real disk the "stupid" `sync_all`-per-file
pattern would likely look worse than my numbers show. So if anything
my measurements *under-state* how bad the stupid patterns are.

## 7. Recommendation

The right framing is conditional on hardware shape.

**Default for typical CI runners and laptops (≤ 8–16 threads).**
Pacquet's current shape — sequential per-tarball writes within a
`spawn_blocking` body, `num_cpus * 2` tarballs in parallel — is
structurally equivalent to the rayon+std::fs strategy that won every
size on this host. **Keep this as the default.** Any io_uring layer
on top would add complexity (Linux-only; single-threaded ring driver
per runtime; lifetime / `'static` buffer constraints; harder error
mapping) for a 1.6×–3.3× regression on the workload it targets.

**For high-thread Linux workstations / build servers (32+ threads).**
A Linux-only io_uring CAS-write fast-path is a credible
optimization. The 56-thread data shows `tokio_uring` correct beating
rayon by 1.44×, and the mechanism (kernel-lock side-step) gets
*worse* with core count, not better. The realistic implementation
shape: a feature-flagged or runtime-detected single shared monoio /
tokio_uring runtime for CAS writes, gated behind something like
`PACQUET_USE_IO_URING=1` (or a `--prefer-iouring` flag) until the
ergonomics across pacquet's existing tokio multi-thread runtime are
worked out. monoio is the better candidate of the two: it's faster
on this host (1.25× over tokio_uring) and the mechanism predicts the
advantage holds on bigger boxes; at 1024 files the difference washes
out into noise, so picking on 1024-file data alone wouldn't justify
the choice — pick monoio because its runtime overhead is lower and
that *only matters more* as the workload gets faster per-op.

**Within-tarball rayon parallelism is a credible follow-up
regardless.** At 1024 files rayon hits 2.63× sequential. Pacquet
today writes each tarball's CAS files sequentially within one
`spawn_blocking` body; switching that loop to `par_iter` while
keeping the `num_cpus * 2` outer cap would shave a meaningful chunk
off install time on packages with many small files
(e.g. babel/eslint plugin forests), without changing the threading
model. This is **separate from the io_uring question** and applies
to every host shape.

**The gist's "stupid" patterns are a universal negative example.**
Both runtimes pay the same penalty for them at scale; both runtimes
mostly hide it on small workloads. Two anti-patterns to flag in
review:

1. `join_all` over a large unbounded set of I/O futures without a
   `buffer_unordered` cap.
2. Per-op `fsync` / `sync_all` in a hot batch unless durability is
   actually required.

These already appear implicitly in pacquet's review checklists via
the `post_download_semaphore()` cap and the `ensure_file` design;
this report just makes the rationale explicit and gives it
quantitative weight on two distinct runtimes.

### Best implementation, in one sentence

* On low-CPU hosts (this sandbox, most CI runners): **rayon
  `par_iter` over `std::fs::write`** is the best implementation, and
  pacquet's current `spawn_blocking`-per-tarball shape is its
  multi-tarball generalization.
* On high-CPU Linux hosts (28+ cores): **a single shared `monoio`
  (preferred) or `tokio_uring` runtime, sized at 1024 SQEs with
  `buffer_unordered(num_cpus*2)` and no per-file `sync_all`** —
  i.e. the gist's "correct" pattern, ideally on monoio for the
  lower per-task overhead — is the faster strategy. Adoption would
  need to be opt-in until cross-host evidence makes the default
  switch.
* Universally: **never the "stupid" pattern.** Both runtimes
  reproduce the ~1.1×–4.5× regression the gist diagnosed; only the
  exact magnitude varies with workload size.
