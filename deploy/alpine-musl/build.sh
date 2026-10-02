#!/bin/sh
#
# Build feldspar natively on Alpine / musl:
#
#     deploy/alpine-musl/build.sh                 # cargo build --release -p sc-cli
#     deploy/alpine-musl/build.sh -j 2            # extra args go to cargo
#     SC_BUILD_ADMIN=0 deploy/alpine-musl/build.sh   # skip the npm UI bundles
#
# Runs setup.sh (idempotent) first. Output: target/release/feldspar, a
# dynamically linked musl binary. See docs/BUILD_ALPINE_MUSL.md.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

sh "$HERE/setup.sh" ${SC_NO_APK:+--no-apk}
. "$HERE/env.sh"
exec cargo build --release -p sc-cli "$@"
