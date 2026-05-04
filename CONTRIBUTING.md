# Contributing to pacquet

See also [`CODE_STYLE_GUIDE.md`](./CODE_STYLE_GUIDE.md) for the code style guide.

## Commit Message Convention

This project uses [Conventional Commits](https://www.conventionalcommits.org/).

### Format

```
type(scope): lowercase description
```

### Rules

- **Types:** `feat`, `fix`, `refactor`, `perf`, `docs`, `style`, `chore`, `ci`, `test`, `lint`.
- **Scopes** (optional): a crate name (`cli`, `store`, `tarball`, `registry`, `lockfile`, `npmrc`, `network`, `fs`, `package-manager`, etc.), or another relevant area such as `deps`, `readme`, `benchmark`, or `toolchain`.
- **Description:** always lowercase after the colon, no trailing period, brief (3-7 words preferred).
- **Breaking changes:** append `!` before the colon. For example: `feat(cli)!: remove deprecated flag`.
- **Code identifiers** in descriptions should be wrapped in backticks. For example: `` chore(deps): update `serde` ``.

There are no exceptions to this format. Version release commits follow the same rules as any other commit.

## Writing Style

Write documentation, comments, and other prose for ease of understanding first. Prefer a formal tone when it does not hurt clarity, and use complete sentences. Avoid mid-sentence breaks introduced by em dashes or long parenthetical clauses. Em dashes are a reliable symptom of loose phrasing; when one appears, restructure the surrounding sentence so each clause stands on its own rather than swapping the em dash for another punctuation mark.

## Code Style

See [`CODE_STYLE_GUIDE.md`](./CODE_STYLE_GUIDE.md). Formatting and lint-level rules are enforced by `cargo fmt`, `taplo format`, and `cargo clippy`; the style guide covers everything those tools cannot enforce.

## Setup

### Prerequisites

Install these manually before running any of the commands below:

- **Rust toolchain** — managed by [`rustup`](https://rustup.rs). The version is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml); `rustup` reads it automatically when you run `cargo` inside the repo.
- **[`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall)** — required by `just init` to fetch prebuilt binaries for the cargo-based tools listed below. Install with `cargo install cargo-binstall` or download a release binary from the project page.
- **[`just`](https://just.systems)** — the task runner used throughout this guide. Install with `cargo install just`, `cargo binstall just`, or your system package manager.
- **Node.js** — required by the mocked registry (`tasks/registry-mock/`), which runs `verdaccio` from npm. Any Node.js release supported by the pinned pnpm works.
- **[`pnpm`](https://pnpm.io)** — used by `just install` to populate `tasks/registry-mock/node_modules/`. The version is pinned in [`package.json`](./package.json) (`devEngines.packageManager`); install via [Corepack](https://nodejs.org/api/corepack.html) or `npm install -g pnpm@<pinned-version>`.
- **`git`** — the repo ships pre-push hooks under [`.githooks/`](./.githooks/) that `just install-hooks` activates.

### Install

Install the project's task tools and the git pre-push hook:

```sh
just init
```

`just init` invokes `cargo-binstall` to install `cargo-nextest`, `cargo-watch`, `cargo-insta`, `typos-cli`, `taplo-cli`, `wasm-pack`, and `cargo-llvm-cov`, then points `git` at the tracked `.githooks/` directory so the pre-push format check runs on `git push`.

Install the test dependencies:

```sh
just install
```

## Automated Checks

Before submitting, run:

```sh
just ready
```

This runs `typos`, `cargo fmt`, `just check` (which is `cargo check --locked`), `just test` (which is `cargo nextest run`), and `just lint` (which is `cargo clippy --locked -- --deny warnings`), then prints `git status`. CI runs the same commands on Linux, macOS, and Windows.

> [!IMPORTANT]
> Run `just ready` before every commit. This rule applies to all changes, including documentation edits, comment changes, and config updates. Any change can break formatting, linting, building, or tests across the supported platforms.

> [!NOTE]
> Some integration tests require the local registry mock. Start it with `just registry-mock launch` before running `just test` if a test needs it.

## Debugging

Set the `TRACE` environment variable to enable trace-level logging for a given module:

```sh
TRACE=pacquet_tarball just cli add fastify
```

## Testing

```sh
just install              # install necessary dependencies
just registry-mock launch # start a mocked registry server (optional)
just test                 # run tests
```

When porting tests from the upstream `pnpm/pnpm` TypeScript repository, see
[`plans/TEST_PORTING.md`](./plans/TEST_PORTING.md). It tracks the tests
scheduled for porting (with upstream file paths and line numbers), the
expected layout for not-yet-implemented behavior (`known_failures` modules
guarded by `pacquet_testing_utils::allow_known_failure!`), and the
verification step of temporarily breaking the implementation to confirm a
ported test actually fails for the right reason before committing.

## Benchmarking

First, start a local registry server, such as [verdaccio](https://verdaccio.org/):

```sh
verdaccio
```

Then use the `integrated-benchmark` task to run benchmarks. For example:

```sh
# Compare the branch you are working on against main
just integrated-benchmark --scenario=frozen-lockfile my-branch main
```

```sh
# Compare the current commit against the previous commit
just integrated-benchmark --scenario=frozen-lockfile HEAD HEAD~
```

```sh
# Compare pacquet of the current commit against pnpm
just integrated-benchmark --scenario=frozen-lockfile --with-pnpm HEAD
```

```sh
# Compare pacquet of the current commit, pacquet of main, and pnpm against each other
just integrated-benchmark --scenario=frozen-lockfile --with-pnpm HEAD main
```

```sh
# See more options
just integrated-benchmark --help
```
