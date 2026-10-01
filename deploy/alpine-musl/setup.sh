#!/bin/sh
#
# One-time (per V8 version) preparation for building feldspar natively on
# Alpine / musl. Idempotent: safe to re-run, and build.sh runs it every time.
#
#   1. installs the Alpine packages the build needs (skip with --no-apk)
#   2. downloads upstream rusty_v8's prebuilt *glibc* archive for the V8 version
#      in Cargo.lock, compiles glibc_shim.c and appends it to a copy
#   3. patches deno_runtime / deno_node in the cargo registry (patch-registry.sh)
#      and `cargo clean`s whatever it changed
#
# Output lives in $SC_MUSL_V8_DIR (default ~/.local/share/feldspar-musl-v8),
# one subdirectory per V8 version. See docs/BUILD_ALPINE_MUSL.md.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
cd "$REPO"

INSTALL_APK=1
[ "${1:-}" = "--no-apk" ] && INSTALL_APK=0

log() { printf '==> %s\n' "$*" >&2; }

if [ "$INSTALL_APK" = 1 ] && command -v apk >/dev/null; then
    # build-base: cc/make for the many -sys crates; clang*-libclang: bindgen;
    # linux-headers: libffi-sys; nodejs/npm: the UI bundles sc-cli's build.rs
    # builds (set SC_BUILD_ADMIN=0 to skip those); python3/curl: v8's build.rs.
    clang_pkg="$(apk search -q 'clang*-libclang' 2>/dev/null | sort -V | tail -1)"
    log "installing Alpine packages (${clang_pkg:-clang-libclang})"
    apk add -q build-base linux-headers python3 curl gzip nodejs npm perl cmake \
        "${clang_pkg:-clang-libclang}"
fi

case "$(uname -m)" in
    aarch64|arm64) arch=aarch64 ;;
    x86_64|amd64)  arch=x86_64 ;;
    *) echo "error: no prebuilt rusty_v8 for $(uname -m)" >&2; exit 1 ;;
esac

version="$(awk '/^name = "v8"$/ { getline; gsub(/"/, "", $3); print $3; exit }' Cargo.lock)"
[ -n "$version" ] || { echo "error: no v8 package in Cargo.lock" >&2; exit 1; }

# The workspace enables the v8 crate's default `simdutf` feature, so this is
# the archive flavour its build.rs would have asked for on glibc.
flavour="${SC_V8_FLAVOUR:-simdutf_release}"
dir="${SC_MUSL_V8_DIR:-$HOME/.local/share/feldspar-musl-v8}/$version"
base="https://github.com/denoland/rusty_v8/releases/download/v$version"
mkdir -p "$dir"

if [ ! -f "$dir/librusty_v8_musl.a" ] || [ "$HERE/glibc_shim.c" -nt "$dir/librusty_v8_musl.a" ]; then
    if [ ! -f "$dir/librusty_v8_gnu.a" ]; then
        log "downloading rusty_v8 $version ($flavour, $arch-unknown-linux-gnu)"
        curl -fL --retry 3 -o "$dir/librusty_v8_gnu.a.gz" \
            "$base/librusty_v8_${flavour}_${arch}-unknown-linux-gnu.a.gz"
        gunzip -f "$dir/librusty_v8_gnu.a.gz"
    fi
    log "compiling glibc_shim.c into the archive"
    cc -O2 -fPIC -c "$HERE/glibc_shim.c" -o "$dir/glibc_shim.o"
    cp "$dir/librusty_v8_gnu.a" "$dir/librusty_v8_musl.a.tmp"
    ar r "$dir/librusty_v8_musl.a.tmp" "$dir/glibc_shim.o" 2>/dev/null
    mv "$dir/librusty_v8_musl.a.tmp" "$dir/librusty_v8_musl.a"
fi

if [ ! -f "$dir/src_binding.rs" ]; then
    log "downloading rusty_v8 $version bindings"
    curl -fL --retry 3 -o "$dir/src_binding.rs" \
        "$base/src_binding_${flavour}_${arch}-unknown-linux-gnu.rs"
fi

# The registry sources exist only after cargo has fetched them.
cargo fetch -q
changed="$(sh "$HERE/patch-registry.sh" | sort -u)"
for crate in $changed; do
    log "patched $crate; cleaning its stale build"
    cargo clean -q --release -p "$crate" 2>/dev/null || true
    cargo clean -q -p "$crate" 2>/dev/null || true
done

log "ready: V8 $version in $dir"
