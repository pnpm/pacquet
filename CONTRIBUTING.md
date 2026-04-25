# Contributing to pacquet

See also [`CODE_STYLE_GUIDE.md`](./CODE_STYLE_GUIDE.md) for code-level conventions that complement this guide.

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

Automated tools enforce formatting (`cargo fmt`, `taplo format`) and linting (`cargo clippy`). The conventions below are **not** enforced by those tools and must be followed manually. See [`CODE_STYLE_GUIDE.md`](./CODE_STYLE_GUIDE.md) for additional rules around ownership, borrowing, and reference-counted cloning.

### Import Organization

Prefer **merged imports**. Combine multiple items from the same crate or module into a single `use` statement with braces rather than separate `use` lines. Import ordering is enforced by `cargo fmt`. Imports gated by a platform attribute such as `#[cfg(unix)]` go in a separate block after the main imports.

```rust
use crate::{
    package_manager::PackageManager,
    store::Store,
};
use pipe_trait::Pipe;
use std::{path::PathBuf, sync::Arc};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
```

### Module Organization

- Use the flat file pattern (`module.rs`) rather than `module/mod.rs` for submodules.
- List `pub mod` declarations first, then `pub use` re-exports, then private imports and items.
- Use `pub use` to re-export key types at the module level for convenience.

```rust
pub mod install_package_by_snapshot;
pub mod install_package_from_registry;
pub mod install_without_lockfile;

pub use install_package_by_snapshot::InstallPackageBySnapshot;
pub use install_package_from_registry::InstallPackageFromRegistry;
```

### Derive Macro Ordering

When deriving multiple traits, use this order and split across multiple `#[derive(...)]` lines for readability:

1. **Standard traits:** `Debug`, `Default`, `Clone`, `Copy`
2. **Comparison traits:** `PartialEq`, `Eq`, `PartialOrd`, `Ord`
3. **Hash**
4. **`derive_more` traits:** `Display`, `From`, `Into`, `Add`, `AddAssign`, etc.
5. **Feature-gated derives** on a separate `#[cfg_attr(...)]` line

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[derive(From, Into, Display)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub struct PackageSize(u64);
```

### Generic Parameter Naming

Use **descriptive names** for type parameters, not single letters:

- `Size`, `Name`, `Manifest`, `Store`, `Reporter`

Single-letter generics are acceptable only in very short, self-contained trait impls.

### Variable and Closure Parameter Naming

Use **descriptive names** for variables and closure parameters by default. Single-letter names are permitted only in the specific cases listed below.

#### When single-letter names are allowed

- **Comparison closures:** `|a, b|` in `sort_by`, `cmp`, or similar two-argument comparison callbacks. This is idiomatic Rust.

  ```rust
  packages.sort_by(|a, b| a.name.cmp(&b.name));
  ```

- **Conventional single-letter names:** `n` for a natural number such as an unsigned integer or count, `f` for a `fmt::Formatter`, and similar well-established conventions from math or the Rust standard library. Note: for indices, use `index`, `idx`, or `*_index` such as `row_index`, not `n`. For `i`/`j`/`k`, see the dedicated rule below.

  ```rust
  fn with_capacity(n: usize) -> Self { todo!() }
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { todo!() }
  ```

- **Index variables (`i`, `j`, `k`):** These may only be used in two contexts: short closures, and index-based loops or iterations. The latter is rare in Rust. In all other cases, use `index`, `idx`, or `*_index`.

  ```rust
  // OK: short closure
  rows.zip(cols).map(|(i, j)| matrix[i][j])

  // OK: index-based loop
  for i in 0..len { /* ... */ }

  // Bad: use a descriptive name instead
  let i = items.iter().position(|item| item.is_active()).unwrap();
  ```

- **Trivial single-expression closures:** A closure whose body is a single field access, method call, or wrapper may use a single letter when the type and purpose are obvious from context.

  ```rust
  .pipe(|x| vec![x])
  ```

- **Fold accumulators:** `acc` for the accumulator and a single letter for the element in trivial folds.

  ```rust
  .fold(PathBuf::new(), |acc, x| acc.join(x))
  ```

- **Test fixtures:** `let a`, `let b`, `let c` for interchangeable specimens with identical roles in equality or comparison tests. Do not use single letters when the variables have distinct roles; use `actual`/`expected` or similar descriptive names instead.

  ```rust
  let a = vec![3, 1, 2].into_iter().collect::<BTreeSet<_>>();
  let b = vec![2, 3, 1].into_iter().collect::<BTreeSet<_>>();
  assert_eq!(a, b);
  ```

#### When single-letter names are NOT allowed

- **Multi-line functions and closures:** Use a descriptive name when a function or closure body spans multiple lines. Examples include a body that contains a `let` binding followed by another expression, or a body with multiple chained operations.

  ```rust
  // Good
  .map(|package| {
      let manifest = package.manifest()?;
      install(&manifest)
  })

  // Bad
  .map(|p| {
      let manifest = p.manifest()?;
      install(&manifest)
  })
  ```

- **`let` bindings in non-test code:** Always use descriptive names.

  ```rust
  // Good
  let manifest = package.manifest()?;
  // Bad
  let m = package.manifest()?;
  ```

- **Function and method parameters:** Always use descriptive names, except for the conventional single-letter names listed above, such as `n` and `f`.

- **Closures with non-obvious context:** When the type or purpose is not immediately clear from the surrounding method chain, use a descriptive name.

  ```rust
  // Good: not obvious what the closure receives
  .filter_map(|entry| match entry { _ => todo!() })

  // Bad: reader must look up what .filter receives
  .filter(|x| x.is_published())
  ```

### Trait Bounds

Prefer `where` clauses over inline bounds when there are multiple constraints:

```rust
impl<Store, Manifest, Reporter> InstallPackage<Store, Manifest, Reporter>
where
    Store: PackageStore + Send + Sync,
    Manifest: AsRef<PackageManifest> + Send,
    Reporter: ProgressReporter + Sync + ?Sized,
{
    /* ... */
}
```

### Error Handling

- Use `derive_more` for error types. Only derive the traits that are actually used:
  - `Display`: derive when the type needs to be displayed, such as when it is printed to stderr or used in format strings.
  - `Error`: derive when the type is used as a `std::error::Error`, such as the error type in `Result` or the source of another error. Not all types with `Display` need `Error`.
  - A type that only needs formatting and not error handling should derive `Display` without `Error`.
- Minimize `unwrap()` in non-test code; use proper error propagation. `unwrap()` is acceptable in tests, and is also acceptable for provably infallible operations when accompanied by a comment explaining the invariant. When deliberately ignoring an error, use `.ok()` and document the rationale.

```rust
#[derive(Debug, Display, Error)]
#[non_exhaustive]
pub enum InstallError {
    #[display("NetworkFailure: {_0}")]
    NetworkFailure(reqwest::Error),
}
```

### Conditional Test Skipping: `#[cfg]` vs `#[cfg_attr(..., ignore)]`

When a test cannot run under certain conditions, such as on the wrong platform, prefer `#[cfg_attr(..., ignore)]` over `#[cfg(...)]` to skip it. The test still compiles on every configuration and is only skipped at runtime. This approach catches type errors and regressions that a `#[cfg]` skip would hide.

Use `#[cfg]` on tests **only** when the code cannot compile under the condition. An example is a test that references types, functions, or trait methods gated behind `#[cfg]` that do not exist on other platforms or feature sets.

Prefer including a reason string in the `ignore` attribute to explain why the test is skipped.

```rust
// Good: test compiles everywhere, skipped at runtime on non-unix
#[test]
#[cfg_attr(not(unix), ignore = "only one path separator style is tested")]
fn unix_path_logic() { /* uses hardcoded unix paths but no unix-only types */ }

// Good: test CANNOT compile on non-unix (uses unix-only types)
#[cfg(unix)]
#[test]
fn unix_permissions() { /* uses PermissionsExt which only exists on unix */ }
```

### Using `pipe-trait`

This codebase uses the [`pipe-trait`](https://docs.rs/pipe-trait) crate. The `Pipe` trait enables method-chaining through unary functions, keeping code in a natural left-to-right reading order. Import it as `use pipe_trait::Pipe;`.

Any callable that takes a single argument works with `.pipe()`. This includes free functions, closures, newtype constructors, enum variant constructors, `Some`, `Ok`, `Err`, and trait methods such as `From::from`. The guidance below applies equally to all of them.

#### When to use pipe

**Chaining through a unary function at the end of an expression chain:**

```rust
// Good: pipe keeps the chain flowing left-to-right
manifest.dependencies().pipe(DependencyMap)
entries.into_iter().collect::<HashMap<_, _>>().pipe(Store)
```

**Avoiding deeply nested function calls:**

```rust
// Nested calls are harder to read
let parsed = serde_json::from_slice::<Manifest>(&bytes)?;

// Prefer piping instead
let parsed = bytes.as_slice().pipe(serde_json::from_slice::<Manifest>)?;
```

**Chaining through multiple unary functions:**

```rust
name.pipe(InstallError::MissingPackage).pipe(Err)
```

**Continuing a method chain through a free function and back to methods:**

```rust
url
    .pipe(normalize_registry_url)
    .map(Cow::Borrowed)
```

**Using `.pipe_as_ref()` to pass a reference mid-chain.** This avoids introducing a temporary variable when a free function takes `&T`:

```rust
// Good: pipe_as_ref calls .as_ref() then passes to the function
path_buf.pipe_as_ref(Path::exists)
```

#### When NOT to use pipe

**Simple standalone function calls.** Pipe adds noise with no readability benefit:

```rust
// Bad: unnecessary pipe
let result = value.pipe(foo);

// Good: just call the function directly
let result = foo(value);
```

This applies to any unary callable, such as `Some`, `Ok`, or constructors, when there is no preceding chain to continue:

```rust
// Bad: pipe adds nothing here
let result = value.pipe(Some);

// Good: direct call is clearer
let result = Some(value);
```

However, piping through any unary function **is** preferred when it continues an existing chain:

```rust
// Good: continues a chain
manifest.summarize().pipe(Some)
```

### Pattern Matching

When mapping enum variants to values, prefer the concise wrapping style:

```rust
ExitCode::from(match self {
    InstallError::NetworkFailure(_) => 2,
    InstallError::ManifestParseFailure(_) => 3,
})
```

## Unit Tests

A unit-test module may either sit inline as `mod tests { ... }` in its parent or live in a dedicated external `tests` submodule. Use the inline form for short test modules. Once the block becomes long enough to obscure the surrounding module, move the tests into an external file.

### When the inline form is acceptable

The inline form `mod tests { ... }` is acceptable on its own. Reserve it for modules whose entire test suite fits in a small number of lines, so the block does not noticeably extend the parent. Use the number of lines as the deciding factor.

### Where the external file sits

When the tests live externally, the parent declares them at the end of the file with the standard declaration:

```rust
#[cfg(test)]
mod tests;
```

The external file itself sits in a directory named after the parent, using the same path regardless of whether the parent has any other submodules. Concretely:

- For `src/foo.rs`, the tests file is `src/foo/tests.rs`.
- For `src/foo/bar.rs`, the tests file is `src/foo/bar/tests.rs`.

Do not flatten the tests into a sibling file such as `src/foo_tests.rs`, and do not skip the intermediate directory when the parent currently has no other submodules. This mirrors the flat file pattern (`module.rs` rather than `module/mod.rs`) described under [Module Organization](#module-organization).

## Setup

Install the Rust toolchain pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). Then install the project's task tools and the git pre-push hook:

```sh
just init
```

`just init` requires [`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall). It installs `cargo-nextest`, `cargo-watch`, `cargo-insta`, `typos-cli`, `taplo-cli`, `wasm-pack`, and `cargo-llvm-cov`, then points `git` at the tracked `.githooks/` directory so the pre-push format check runs on `git push`.

Install the registry-mock dependencies before running tests:

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
