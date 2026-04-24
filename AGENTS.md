# AGENTS.md

Guidance for AI coding agents working in this repository.

## What this project is

`pacquet` is a port of the [pnpm](https://github.com/pnpm/pnpm) CLI from
TypeScript to Rust. It is not a new package manager and not a reimagining —
its behavior, flags, defaults, error codes, file formats, and directory layout
are meant to match pnpm exactly.

## The cardinal rule

**Any change in this repo must match how the same feature is implemented in
`pnpm/pnpm` on the latest `main` branch.**

Before writing code for a feature, bug fix, or behavior change:

1. Find the equivalent code in `pnpm/pnpm` on `main`
   (https://github.com/pnpm/pnpm). The TypeScript source lives under `pnpm/`
   (workspaces such as `pnpm/lockfile/`, `pnpm/store/`, `pnpm/cli/`, etc.).
2. Read the upstream implementation — logic, edge cases, config resolution,
   error messages, file/lockfile formats, and existing tests.
3. Port the behavior faithfully. Prefer structural similarity (same function
   decomposition, same names where reasonable) so future cross-referencing
   stays cheap.
4. Do not invent behavior that pnpm does not have. Do not "fix" pnpm quirks
   unless the same fix has landed upstream.
5. If pnpm's `main` and this repo disagree, pnpm's `main` is the source of
   truth — reconcile toward upstream, not away from it.

If the upstream behavior is unclear or looks wrong, stop and ask the user
rather than guessing.

### Internal performance divergence is allowed

The cardinal rule governs **observable behavior**: CLI flags, defaults,
error codes, error messages, `.npmrc` semantics, lockfile format, CAS /
store-index layout, and anything else a user or a coexisting pnpm install
can see. Internal implementation details — pipeline topology, async vs.
sync, streaming vs. buffering, hash-tee vs. post-hoc hashing, thread-pool
shape, chunk sizes, parallelism strategy — are **not** observable and may
diverge from pnpm when the divergence delivers a measured performance win
and leaves observable outputs (bytes in the CAS, rows in `index.db`, error
codes, exit status, stdout/stderr contents) byte-identical.

When you take such a divergence, document it in the commit or the code
with a `Why we diverge:` note: what the upstream shape is, what the pacquet
shape is, and the benchmark delta that justifies it. That keeps the
divergence auditable and reversible if upstream later adopts a different
approach.

When citing upstream code in a PR description or commit message, link to a
specific commit on `main` (not a branch tip) so the reference stays stable.

## Repo layout

- `crates/` — library and binary crates that make up pacquet.
  - `cli`, `package-manager`, `package-manifest`, `lockfile`, `store-dir`,
    `tarball`, `registry`, `network`, `npmrc`, `fs`, `executor`,
    `diagnostics`, `testing-utils`.
- `tasks/` — developer tooling: `integrated-benchmark`, `micro-benchmark`,
  `registry-mock`.
- `CODE_STYLE_GUIDE.md` — Rust style conventions beyond what clippy enforces.
  Read it before submitting code.
- `justfile` — canonical commands (see below).

## Commands

Prefer `just` recipes when one fits; drop down to `cargo` / `taplo` / etc.
directly when you need flags the recipe doesn't expose (e.g. filtering tests
by crate or name — see below).

- `just ready` — run the same checks CI runs (typos, fmt, check, test, lint).
  Run this before declaring a task complete.
- `just test` — `cargo nextest run`.
- `just lint` — `cargo clippy --locked -- --deny warnings`.
- `just check` — `cargo check --locked`.
- `just fmt` — `cargo fmt` + `taplo format`.
- `just cli -- <args>` — run the pacquet binary.
- `just registry-mock <args>` — manage the mock registry used by tests.
- `just integrated-benchmark <args>` — compare revisions or compare against
  pnpm itself (see `README.md`).

Warnings are errors (`--deny warnings` in lint). Do not silence them with
`#[allow(...)]` unless there is a specific, justified reason.

## Tests

- Tests live alongside the code they exercise (standard Cargo layout) plus
  integration tests under each crate's `tests/`. Shared test fixtures live
  under `crates/testing-utils/src/fixtures/`.
- Snapshot tests use `insta`. When an intentional change alters a snapshot,
  review the diff carefully, then accept with `cargo insta review`. Never
  accept snapshot changes blindly.
- Some tests require the mocked registry. Start it with
  `just registry-mock launch` if a test needs it.
- When porting behavior from pnpm, port the relevant pnpm tests too (as Rust
  tests) whenever they translate. Matching test coverage is the easiest way
  to prove behavioral parity.

### Running tests narrowly

Running the full suite is slow. While iterating, target what you're working
on:

```sh
# One crate
cargo nextest run -p pacquet-lockfile

# One test by name substring
cargo nextest run -p pacquet-lockfile <name_substring>

# One integration test file
cargo nextest run -p pacquet-lockfile --test <file_stem>
```

Run `just ready` (full suite) before handing the PR off.

**Never ignore a test failure.** Do not dismiss a failing test as "pre-existing"
or "unrelated to my change." Investigate every failure. If a test was already
broken on `main`, fix it as part of your work rather than silently skipping it
or treating the red as acceptable.

## Style

`CODE_STYLE_GUIDE.md` is the source of truth. Highlights:

- Choose owned vs. borrowed parameters to minimize copies; widen to the most
  encompassing type (`&Path` over `&PathBuf`, `&str` over `&String`) when it
  doesn't force extra copies.
- Prefer `Arc::clone(&x)` / `Rc::clone(&x)` over `x.clone()` for reference-
  counted types, so the cost is visible at the call site.
- Follow the test-logging guidance in the style guide — log before non-
  `assert_eq!` assertions, `dbg!` complex structures, skip logging for simple
  scalar `assert_eq!`.
- Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/naming.html)
  for naming.

## Code reuse and avoiding duplication

This is a small workspace, but it is still a workspace — duplication is still
a risk, especially between crates that touch the filesystem, the store, or
package manifests.

- **Search before you write.** Before implementing any non-trivial helper,
  grep the workspace for existing functions or utilities that do the same
  or a similar thing. Shared helpers tend to live in `crates/fs`,
  `crates/testing-utils`, and `crates/diagnostics`.
- **Extract shared code.** If logic you need already exists in another crate
  but isn't exported, refactor it into a shared crate (or move it to one of
  the utility crates above) rather than copy-pasting.
- **Prefer well-maintained crates over custom implementations.** Don't
  reimplement what a mature crate already provides. Check whether the
  workspace already depends on something suitable (see
  `[workspace.dependencies]` in the root `Cargo.toml`) before adding a new
  dependency.
- **Keep dependencies at the right level.** Add a new dependency to the
  specific crate that needs it, not to the workspace root or to a shared
  crate unless multiple crates actually depend on it.

## Errors and diagnostics

User-facing errors go through `miette` via the `pacquet-diagnostics` crate.
Match pnpm's error codes and messages where pnpm defines them — error codes
are part of the public contract, not implementation detail. See
<https://pnpm.io/errors> for the canonical list.

## Commit and PR hygiene

- Keep commits focused. A bug fix commit should not also refactor or
  reformat unrelated code.
- Reference the upstream pnpm commit/PR you ported from, when applicable.
- Run `just ready` before pushing.
- The repo installs a pre-push hook via `just install-hooks` that runs
  `rustfmt` and `taplo`. Make sure your environment can run cargo (the
  hook needs it) before pushing.

### Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/). Use a
scope that names the crate or area being touched, matching the existing
history (`git log --oneline` for examples). Common types:

- `feat`: a new feature
- `fix`: a bug fix
- `perf`: a performance improvement
- `refactor`: code change that neither fixes a bug nor adds a feature
- `test`: adding or adjusting tests
- `docs`: documentation only
- `chore`: build tooling, CI, or auxiliary changes
- `bench`: benchmark-only changes

Examples (from this repo's history):

```
fix(network): set explicit timeouts on default reqwest client
feat(lockfile): support npm-alias dependencies in snapshots
perf(store-dir): share one read-only StoreIndex across cache lookups
```

## Things not to do

- Do not add features, flags, or behaviors that pnpm does not have.
- Do not change lockfile format, store layout, `.npmrc` semantics, or CLI
  surface unless pnpm changed them first.
- Do not add dependencies casually — check `deny.toml` and prefer crates
  already in the workspace.
- Do not introduce `unsafe` without a clear justification and review.
- Do not disable lints, tests, or CI checks to make a PR green.
