# pacquet

Experimental package manager for node.js written in rust.

TODO:

- [x] Tarball installation & extraction
- [ ] Install all dependencies of a package
- [ ] Update package.json
- [ ] Create a shrink file like `pnpm-lock.json` or `package-lock.json`

Commands:

- [x] init: `cargo run -- init`
- [ ] add: `cargo run -- add fast-querystring`
  - [ ] flags... 
- [ ] remove
- [ ] run


## Prepare environment

1. Create `node_modules/.pacquet` folder which is the virtual store path.

## Add package

For a given package (example fastify) and a version (v1.0.0):

**Variables**:
- `$STORE=node_modules/.pacquet`
- `$PKG=fastify`
- `$VERSION=1.0.0`

**Steps**:
- Install package to `$STORE/$PKG@$VERSION/node_modules/$PKG`
- If there is a dependency of this package:
  1. Create an empty folder at path `$STORE/fastify@$VERSION/node_modules/$PKG/node_modules`
- Symlink `node_modules/$PKG` to `STORE/$PKG@$VERSION/node_modules/$PKG`
- For every dependency of `$PKG`:
  1. Set `$PKG` to dependency name
  2. Set `$VERSION` to `dependency version
  3. Run installation steps from the beginning
- Update package.json with appropriate version
  1. If package.json is not defined, create a file with only `dependencies`
- Update/create shrink file with appropriate ranges

**Disclaimer**: This is mostly a playground for me to learn Rust and understand how package managers work.
