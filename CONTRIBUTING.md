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

Install the Rust toolchain pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). Then install the project's task tools and the git pre-push hook:

```sh
just init
```

`just init` requires [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall). It installs `cargo-nextest`, `cargo-watch`, `cargo-insta`, `typos-cli`, `taplo-cli`, `wasm-pack`, and `cargo-llvm-cov`, then points `git` at the tracked `.githooks/` directory so the pre-push format check runs on `git push`.

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
