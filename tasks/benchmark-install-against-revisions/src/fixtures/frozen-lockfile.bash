#!/bin/bash
set -o errexit -o nounset -o pipefail
cd "$(dirname "$0")"
exec ./pacquet/target/release/pacquet install --frozen-lockfile
