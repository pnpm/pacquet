# pacquet

Experimental package manager for node.js written in rust.

**Disclaimer**: This is mostly a playground for me to learn Rust and understand how package managers work.

### TODO

- [x] `.npmrc` support (for supported features [readme.md](./crates/npmrc/README.md))
- [x] CLI commands (for supported features [readme.md](./crates/cli/README.md))
- [ ] Global store support
- [ ] Shrink-file support in sync with `pnpm-lock.yml`
- [ ] Workspace support
- [ ] Full sync with [pnpm error codes](https://pnpm.io/errors)
- [ ] Generate a `node_modules/.bin` folder

## Debugging

```shell
TRACE=pacquet_tarball cargo run -- add fastify
```

