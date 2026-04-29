# Porting Guide

Cross-cutting rules for porting features from `pnpm/pnpm` into pacquet.
Treat these as a checklist when adding any code that calls into the
filesystem, the environment, or other process-global state.

The cardinal porting rule itself (mirror pnpm v11 exactly) lives in
[`AGENTS.md`](../AGENTS.md). This guide covers *how* to structure ports
so they stay testable as pacquet grows.

## Dependency injection

Pacquet keeps unit tests hermetic by routing every side-effecting,
process-global capability through a trait, and threading a single
provider type through the call graph. The pattern is the one
documented at length in
[pnpm/pacquet#332][di-comment]; this section is the in-tree summary.

[di-comment]: https://github.com/pnpm/pacquet/pull/332#issuecomment-4345054524

### Seven design rules

1. **One trait per capability.** Each side-effecting operation gets
   its own trait. Functions bind only what they consume; test fakes
   implement only what gets called. No lumped
   `FsApi { read, mkdir, write, ... }` with `unreachable!()` filling
   the unused methods.

2. **One generic parameter satisfying multiple bounds.** When a
   function needs several capabilities, compose them:
   `<Api: FsCreateDirAll + FsWrite>`. Resist the multi-parameter form
   (`<Disk: DiskApi, Fs: FsApi>`) — the generic is a *capability
   provider*, not a *capability domain*, so it should never be named
   after one domain.

3. **No `&self` on capability methods unless instance state is
   genuinely needed.** Production impls are unit structs
   (`struct RealApi;`); fakes are unit structs scoped to a test fn.
   Use `static`s declared inside the test fn for per-test scenario
   data — that keeps fakes stateless from the trait's perspective.

4. **Associated types instead of `&self` when a capability operates
   over a data type.** If a capability would otherwise force callers
   to pass an instance just to extract data, lift the data type to
   an associated type. The capability stays a static-method namespace;
   the associated type lets one provider describe how to operate over
   a chosen data shape.

5. **Capability traits live on the implementor, not the data.** The
   trait `FsReadToString` is implemented by `RealApi` (a capability
   provider), not by `Path` (a value). This keeps the capability
   impl swappable without changing call sites.

6. **Provider names are domain-neutral; trait names are domain-scoped.**
   The generic and its production impl are `Api` and `RealApi`
   regardless of how many domains they cover. The traits keep a
   domain prefix (`Fs*`, `GetDisk*`, `Env*`, …) so a reader can see
   which domain a bound belongs to without chasing definitions.

7. **Production callers turbofish the real impl explicitly.**
   `Npmrc::current::<RealApi, _, _, _, _>(...)`. Defaults on free-fn
   type parameters are unstable; an explicit turbofish is the price
   of zero-cost DI on stable Rust.

### Filesystem capabilities (illustrative)

The same shape covers filesystem operations the way
[`pacquet-modules-yaml`][pr-332] is going to introduce them:

[pr-332]: https://github.com/pnpm/pacquet/pull/332

```rust
pub trait FsReadToString {
    fn read_to_string(path: &Path) -> io::Result<String>;
}

pub trait FsCreateDirAll {
    fn create_dir_all(path: &Path) -> io::Result<()>;
}

pub trait FsWrite {
    fn write(path: &Path, contents: &[u8]) -> io::Result<()>;
}

// One provider for the whole codebase. Today it carries only a few
// capabilities; future PRs add disk, network, time, etc. impls to
// the *same* struct so callers never juggle multiple providers.
pub struct RealApi;
impl FsReadToString for RealApi { /* delegates to std::fs::read_to_string */ }
impl FsCreateDirAll for RealApi { /* delegates to std::fs::create_dir_all */ }
impl FsWrite        for RealApi { /* delegates to std::fs::write           */ }
```

Public API binds minimal capabilities:

```rust
pub fn read_modules_manifest<Api: FsReadToString>(...) -> Result<...>
pub fn write_modules_manifest<Api: FsCreateDirAll + FsWrite>(...) -> Result<...>
```

Test fakes describe what behaviour they fake, not their provider role:

```rust
struct BadYamlFs;
impl FsReadToString for BadYamlFs {
    fn read_to_string(_: &Path) -> io::Result<String> {
        Ok("{ this is not valid yaml or json".to_string())
    }
}
let err = read_modules_manifest::<BadYamlFs>(modules_dir).expect_err("expected");
```

The fake declares **only** `FsReadToString` because
`read_modules_manifest`'s bound is `Api: FsReadToString`. No `FsWrite`
or `FsCreateDirAll` impls are needed; no `unreachable!()` guards.

### Environment-variable capability (this PR)

`pacquet-npmrc` introduces the `EnvVar` capability for `${VAR}`
substitution inside `.npmrc`:

```rust
pub trait EnvVar {
    fn var(name: &str) -> Option<String>;
}

pub struct RealApi;
impl EnvVar for RealApi {
    fn var(name: &str) -> Option<String> { std::env::var(name).ok() }
}
```

Callers parameterise on `Api: EnvVar`:

```rust
pub fn env_replace<Api: EnvVar>(text: &str) -> Result<String, EnvReplaceError>
pub fn from_ini<Api: EnvVar>(text: &str) -> NpmrcAuth
pub fn current<Api: EnvVar, ...>(current_dir: ..., home_dir: ..., default: ...) -> Self
```

Tests substitute a stateless fake:

```rust
struct EnvWithToken;
impl EnvVar for EnvWithToken {
    fn var(name: &str) -> Option<String> {
        (name == "TOKEN").then(|| "abc123".to_owned())
    }
}
assert_eq!(env_replace::<EnvWithToken>("Bearer ${TOKEN}").unwrap(), "Bearer abc123");
```

When test scenarios need varying data, store it in a `static` inside
the test fn — keep the fake itself stateless:

```rust
static ENV: &[(&str, &str)] = &[("A", "1"), ("B", "2")];
struct StaticEnv;
impl EnvVar for StaticEnv {
    fn var(name: &str) -> Option<String> {
        ENV.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
    }
}
```

`RealApi` lives in `pacquet-npmrc` for now and gathers a second impl
each time a new domain joins the codebase. When more crates need it,
promote `RealApi` to a shared crate so the symbol stays one
provider, not many.

### When DI is not needed

Don't reach for DI for:
* anything where the real filesystem can produce the input cheaply
  via fixture files in `tests/fixtures/`,
* anything that exercises only pure logic — `is_present_string`,
  `derive_hoisted_dependencies`, etc.,
* stateful runtime objects (a `ThrottledClient`, a `StoreIndex`, a
  `MemCache`); pass those as ordinary arguments.

DI is for unreachable-via-real-fs error paths and for behaviour
whose triggering conditions are awkward to set up on disk or in the
real environment.

### Closures for stateless inputs that already used them

Two arguments to `Npmrc::current` predate the trait pattern and
remain closures:

```rust
pub fn current<Api, Error, CurrentDir, HomeDir, Default>(
    current_dir: CurrentDir,
    home_dir: HomeDir,
    default: Default,
) -> Self
where
    Api: EnvVar,
    CurrentDir: FnOnce() -> Result<PathBuf, Error>,
    HomeDir: FnOnce() -> Option<PathBuf>,
    Default: FnOnce() -> Npmrc,
```

`current_dir` and `home_dir` are one-shot lookups whose error
shape is upstream-defined (`io::Error`, `Option`), so wrapping them
in a trait would not unlock additional test branches. They will
move into the trait pattern alongside any future capability that
they need to compose with — but until then, leaving them as
closures avoids an extra blank refactor.

## Citing upstream

Every reference to `pnpm/pnpm` in pacquet code, comments, docs, or
commit messages must use a permanent commit-SHA link, not a branch
link. See [`AGENTS.md`](../AGENTS.md#the-cardinal-rule) for the rule
and how to resolve a SHA. Use the first 10 hex characters.

## Running the porting workflow

1. Find the upstream source on `pnpm/pnpm`'s `main` branch, pin the
   SHA, and link it from comments.
2. Port the behaviour, structurally close to the original where
   reasonable.
3. Port the upstream tests too whenever they translate. See
   [`TEST_PORTING.md`](./TEST_PORTING.md) for the running list of
   tests still to port for stage 1.
4. Inject every process-global side effect the new code reads
   through a per-capability trait, per the rules above, so unit
   tests can drive each branch with a stateless fake.
5. Run `just ready` before pushing.
