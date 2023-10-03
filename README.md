# pacquet

Experimental package manager for node.js written in rust.

### TODO

- [x] `.npmrc` support (for supported features [readme.md](./crates/npmrc/README.md))
- [x] CLI commands (for supported features [readme.md](./crates/cli/README.md))
- [x] Content addressable file store support
- [ ] Shrink-file support in sync with `pnpm-lock.yml`
  - [x] Frozen lockfile
  - [ ] Update outdated lockfile
  - [ ] Creating lockfile
- [ ] Workspace support
- [ ] Full sync with [pnpm error codes](https://pnpm.io/errors)
- [ ] Generate a `node_modules/.bin` folder
- [ ] Add CLI report

## Debugging

```shell
TRACE=pacquet_tarball just cli add fastify
```

## Benchmarking

### Install between multiple revisions

First, you to start a local registry server, such as [verdaccio](https://verdaccio.org/):

```sh
verdaccio
```

Then, you can use the script named `benchmark-install-against-revisions` to run the various benchmark, For example:

```sh
# Comparing the branch you're working on against main
cargo benchmark-install-against-revisions --scenario=frozen-lockfile my-branch main
```

```sh
# Comparing current commit against the previous commit
cargo benchmark-install-against-revisions --scenario=frozen-lockfile HEAD HEAD~
```

```sh
# Comparing pacquet of current commit against pnpm
cargo benchmark-install-against-revisions --scenario=frozen-lockfile --with-pnpm HEAD
```

```sh
# Comparing pacquet of current commit, pacquet of main, and pnpm against each other
cargo benchmark-install-against-revisions --scenario=frozen-lockfile --with-pnpm HEAD main
```

```sh
# See more options
cargo benchmark-install-against-revisions --help
```
