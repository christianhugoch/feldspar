#!/bin/sh
#
# Patch the local cargo-registry copies of two deno crates so they compile and
# run on musl. Touches ~/.cargo/registry only, never the repository.
#
# Idempotent. Prints the name of every crate it actually changed, one per line,
# so the caller can `cargo clean -p` it: cargo does not notice edits to
# registry sources and would otherwise keep the stale compiled crate.
#
# See docs/BUILD_ALPINE_MUSL.md for why each patch exists.
set -eu
R="${CARGO_HOME:-$HOME/.cargo}/registry/src"

# 1. deno_runtime calls glibc-only `libc::malloc_trim` (absent from the libc
#    crate on musl: a compile error). Gate each call on target_env = "gnu".
for f in "$R"/*/deno_runtime-*/ops/worker_host.rs "$R"/*/deno_runtime-*/worker.rs; do
    [ -f "$f" ] || continue
    grep -q '^ *libc::malloc_trim(0);' "$f" || continue
    sed -i 's/^\( *\)libc::malloc_trim(0);/\1#[cfg(target_env = "gnu")] libc::malloc_trim(0);/' "$f"
    echo deno_runtime
done

# 2. deno_node registers an `.init_array` hook taking (argc, argv, envp). glibc
#    passes those to constructors; musl passes nothing, so the hook reads
#    garbage and every binary linking deno_node segfaults before main —
#    first visible as `sc-module`'s build script dying with SIGSEGV. Gate it
#    on glibc; on musl Node's `process.title = …` becomes a no-op.
for f in "$R"/*/deno_node-*/ops/process.rs; do
    [ -f "$f" ] || continue
    grep -q -A1 '^#\[cfg(target_os = "linux")\]$' "$f" || continue
    grep -A1 '^#\[cfg(target_os = "linux")\]$' "$f" | grep -q '^#\[used\]$' || continue
    awk '
        /^#\[cfg\(target_os = "linux"\)\]$/ { held = $0; next }
        held != "" {
            if ($0 ~ /^#\[used\]$/) print "#[cfg(all(target_os = \"linux\", target_env = \"gnu\"))]"
            else print held
            held = ""
        }
        { print }
    ' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    echo deno_node
done
