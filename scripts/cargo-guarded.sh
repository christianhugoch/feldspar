#!/usr/bin/env bash
#
# Run a cargo command with a hard ceiling on how much memory it can take from
# the rest of the machine.
#
# Why this exists
# ---------------
# This workspace links a static V8 (`deno_core`, behind `sc-expr`'s `eval`
# feature) into **every one** of its ~110 integration-test binaries, so
# `cargo test --workspace` is a burst of very large, very parallel links. On a
# desktop running `systemd-oomd` that burst is dangerous in a specific and
# unobvious way: oomd watches *memory pressure* on `user@1000.service`, and when
# it trips it kills the heaviest **cgroup** underneath — not the heaviest
# process. A terminal's shell, cargo, every rustc and the tab itself all live in
# one `vte-spawn-*.scope`, so "cargo used too much memory" is executed as "close
# the terminal window", losing the scrollback that would have explained it.
#
# `systemd-run --user --scope` puts the build in its own transient scope, a
# *sibling* of the terminal's scope rather than a child:
#
#   app.slice/app-org.gnome.Terminal.slice/vte-spawn-….scope   <- the tab
#   app.slice/run-p1262948-i1274491.scope                      <- the build
#
# The build is now the only thing in its cgroup, so both the kernel and oomd can
# only take the build. `MemoryHigh` throttles it into reclaim first, which is
# usually enough to ride out a spike; `MemoryMax` is the wall behind that. A
# build killed at the wall reports it and exits non-zero — the terminal, and the
# output explaining what happened, stay.
#
# This is a blast-radius guard, not the fix. The fix is the debug-info budget in
# the workspace `Cargo.toml`, which is what keeps a run from approaching these
# numbers at all.
#
# Usage
# -----
#   scripts/cargo-guarded.sh test --workspace
#   scripts/cargo-guarded.sh clippy --workspace --all-targets
#
#   SC_BUILD_MEM_MAX=8G scripts/cargo-guarded.sh test --workspace
#
# On a machine without systemd (macOS, WSL1, a container) it runs cargo
# directly, so it is always safe to use in place of `cargo`.

set -euo pipefail

# Default ceiling: leave the desktop a working machine while the suite runs.
# When this was written a full `cargo test --workspace` from a cold `target/`,
# with the debug-info budget in place, peaked at ~3.3 GB of toolchain RSS and
# 12 GB was roughly 4x headroom. The tree has grown a great deal since — the npm
# module runtime links the whole of `deno_runtime`, taking the largest test
# binary back to ~440 MB — and a cold `--workspace` run at the default `-j8` now
# reaches this ceiling and is killed. That is the wrapper working: `-j4` fits,
# and so does a bigger `SC_BUILD_MEM_MAX` on a machine with the memory to spare.
# The number stays where it is because its job is to protect the rest of the
# session, not to accommodate whatever the build has grown into.
: "${SC_BUILD_MEM_MAX:=12G}"
: "${SC_BUILD_MEM_HIGH:=10G}"

if [ "$#" -eq 0 ]; then
    echo "usage: $(basename "$0") <cargo-subcommand> [args...]" >&2
    exit 2
fi

# `--user` needs a session bus to talk to; cron jobs and some CI shells have
# systemd-run on PATH but no bus, and failing there would be worse than not
# guarding at all.
#
# The grouping is explicit because `||` and `&&` bind left-to-right with equal
# precedence in sh: written flat, "no systemd-run OR (no bus var AND no bus
# socket)" would parse as "(no systemd-run OR no bus var) AND no bus socket",
# which stops falling back on exactly the machine that has no systemd-run.
if ! command -v systemd-run >/dev/null 2>&1 ||
    { [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && [ ! -S "/run/user/$(id -u)/bus" ]; }; then
    exec cargo "$@"
fi

echo "==> cargo $* (capped at MemoryMax=$SC_BUILD_MEM_MAX, MemoryHigh=$SC_BUILD_MEM_HIGH)" >&2

set +e
systemd-run --user --scope --quiet --collect \
    -p "MemoryMax=$SC_BUILD_MEM_MAX" \
    -p "MemoryHigh=$SC_BUILD_MEM_HIGH" \
    -p "MemorySwapMax=2G" \
    -- cargo "$@"
status=$?
set -e

# 137 = SIGKILL. From inside this scope that essentially only happens one way:
# the cgroup hit MemoryMax and the kernel OOM-killer took something in it.
if [ "$status" -eq 137 ]; then
    cat >&2 <<EOF

==> The build was killed at the ${SC_BUILD_MEM_MAX} cgroup limit, not by the system.
    Your terminal and this output survived because the build ran in its own cgroup.

    Options:
      * Lower parallelism:  cargo $* -j4
      * Raise the ceiling:  SC_BUILD_MEM_MAX=20G $0 $*
      * Check the debug-info budget in Cargo.toml is still in effect:
          cargo config get profile   # deps should be debug = false
EOF
fi

exit "$status"
