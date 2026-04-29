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

When citing upstream code anywhere — code comments, doc comments, Markdown
docs, PR descriptions, or commit messages — link to a specific commit SHA, not
a branch name. Branch links such as `github.com/<owner>/<repo>/blob/main/...`
or `.../tree/master/...` are *impermanent*: their target drifts as the branch
moves and may eventually 404 if the file is renamed or deleted. Permanent
links pin the commit (`github.com/<owner>/<repo>/blob/<sha>/...`) so the
reference stays meaningful long after upstream changes. Use the **first 10
hex characters** of the SHA — full 40-character SHAs make URLs unwieldy on
narrow displays and in commit logs, and 10 characters is more than enough to
disambiguate a commit in any real-world repository. Resolve the SHA with
`git ls-remote https://github.com/<owner>/<repo>.git refs/heads/<branch>`
(then take the first 10 characters) or by clicking "Copy permalink" (`y`) on
GitHub and trimming the SHA segment. This rule applies to every GitHub
repository, not only `pnpm/pnpm`.

## Follow the project guides

1. Follow the contributing guide in [`CONTRIBUTING.md`](./CONTRIBUTING.md). It covers commit message format, writing style, setup, and the automated checks to run before committing.
2. Follow the code style guide in [`CODE_STYLE_GUIDE.md`](./CODE_STYLE_GUIDE.md). It covers code-level conventions not enforced by tooling: imports, modules, naming, ownership and borrowing, parameter type selection, trait bounds, pattern matching, `pipe-trait`, error handling, test layout, logging during tests, and cloning of `Arc` and `Rc`.

## Repo layout

- `crates/` — library and binary crates that make up pacquet.
  - `cli`, `package-manager`, `package-manifest`, `lockfile`, `store-dir`,
    `tarball`, `registry`, `network`, `npmrc`, `fs`, `executor`,
    `diagnostics`, `testing-utils`.
- `tasks/` — developer tooling: `integrated-benchmark`, `micro-benchmark`,
  `registry-mock`.
- `CONTRIBUTING.md` — commit-message format, writing style, setup, and the
  automated checks to run before submitting. Read it before submitting code.
- `CODE_STYLE_GUIDE.md` — manual code-style conventions beyond what `cargo
  fmt`, `taplo`, and clippy enforce: imports, modules, naming, ownership
  and borrowing, trait bounds, pattern matching, `pipe-trait`, error
  handling, test layout, and `Arc`/`Rc` cloning. Read it before submitting
  code.
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
  pnpm itself (see `CONTRIBUTING.md`).

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

### Preserve existing method chains

When editing existing code, do not break a method chain (including `pipe-trait`
`.pipe(...)` chains) into intermediate `let` bindings unless you can justify
the rewrite. Valid justifications include a chain that fails to compile after
your edit, a borrow checker rejection, a meaningful performance win from
splitting it up, or any other concrete reason the chain cannot stay as it is.
Refactoring for style alone is not a justification when the task is something
else. Keep the surrounding code shape intact and confine your edits to what
the task asks for.

When the change you need can fit inside the existing chain, keep it there.
For example, swapping a `PathBuf::from` allocation for a `Path::new` borrow:

```diff
 output
     .stdout
     .pipe(String::from_utf8)
     .expect("convert stdout to UTF-8")
     .trim_end()
-    .pipe(PathBuf::from)
+    .pipe(Path::new)
     .parent()
     .expect("parent of root manifest")
     .to_path_buf()
```

Do not flatten the chain just because you happen to be editing nearby:

```diff
-output
-    .stdout
-    .pipe(String::from_utf8)
-    .expect("convert stdout to UTF-8")
-    .trim_end()
-    .pipe(PathBuf::from)
-    .parent()
-    .expect("parent of root manifest")
-    .to_path_buf()
+let stdout = String::from_utf8(output.stdout).expect("convert stdout to UTF-8");
+Path::new(stdout.trim_end()).parent().expect("parent of root manifest").to_path_buf()
```

If you do need to break a chain (compiler error, borrow checker, performance),
state the justification in your reply, the commit message, or the PR
description so a reviewer can confirm the rewrite was warranted. If the
rewrite is purely stylistic, raise it with the user as its own change rather
than smuggling it into an unrelated edit.

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
