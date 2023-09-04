#!/bin/bash
set -o errexit -o pipefail -o nounset
cd "$(dirname "$0")"
cargo run --bin=benchmark-install-against-revisions -- "$@"
