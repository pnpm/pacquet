# pacquet

Experimental package manager for node.js written in rust.

**Disclaimer**: This is mostly a playground for me to learn Rust and understand how package managers work.

### Features

- [x] Tarball installation & extraction
- [x] Install all dependencies of a package
- [x] Update package.json
- [ ] Create a shrink file like `pnpm-lock.json` or `package-lock.json`
- [ ] Workspace support
- [ ] `.npmrc` support

### Commands

- [x] `init`
- [x] `add <pkg>`
- [ ] `install`
- [ ] `update`
- [ ] `remove`
- [ ] `audit`
- [ ] `list`
- [ ] `outdated`
- [ ] `why`
- [ ] `licenses`
- [x] `run`
- [x] `test`
- [ ] `exec`

## Debugging

```shell
TRACE=pacquet_tarball cargo run -- add fastify
```

