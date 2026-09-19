#!/usr/bin/env bash
#
# Run the test suite inside a memory budget, instead of taking whatever the
# machine has.
#
# Why this exists
# ---------------
# `cargo test --workspace` is two very different workloads back to back, and
# both of them scale their memory with the core count rather than with the
# memory available:
#
#   the build   every test binary statically links V8 (`deno_core`) and, in
#               `sc-server`, the whole of `deno_runtime`. Cargo compiles and
#               links `nproc` of them at once: a full workspace rebuild
#               measured 6.6 GB of anonymous memory at `-j12` against 3.0 GB at
#               `-j4`, for 66 s against 90 s.
#
#   the run     each binary runs its tests as `nproc` threads, and a test here
#               is not cheap: an `sc-server` test stands up a server, a V8
#               isolate and a database of its own. This phase is the cheaper of
#               the two in this process (~1.2 GB measured across the suite),
#               but the databases are real load on the server behind it.
#
# Neither number is a problem on a build machine and both are a problem on a
# desktop, where the suite is not the only thing running: `systemd-oomd` watches
# pressure on the whole user session and kills the heaviest cgroup under it,
# which is how "the tests used too much memory" arrives as an editor, a browser
# or a terminal window disappearing.
#
# So this script sizes both phases from a budget rather than from `nproc`, and
# runs them under `cargo-guarded.sh`, which puts cargo in a cgroup of its own so
# an overrun can only take the test run.
#
# Usage
# -----
#   scripts/test.sh                     # the whole workspace
#   scripts/test.sh -p sc-server        # anything `cargo test` takes
#   scripts/test.sh -p sc-app pg_pool   # ... including a filter
#
#   SC_TEST_MEM=6G scripts/test.sh      # a tighter budget
#   SC_TEST_JOBS=2 SC_TEST_THREADS=2 scripts/test.sh    # or set them directly
#
# The budget defaults to 60% of what the machine has free, clamped to
# [4 GiB, 12 GiB] — the suite gets most of the slack, the session keeps the
# rest. `--test-threads` and `-j` already on the command line are left alone.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---------------------------------------------------------------------------
# The budget.
# ---------------------------------------------------------------------------

# Accept 8G / 8192M / 8589934592, in that order of likelihood.
to_mib() {
    local v="${1^^}"
    case "$v" in
    *G) echo $((${v%G} * 1024)) ;;
    *M) echo "${v%M}" ;;
    *) echo $(($v / 1048576)) ;;
    esac
}

available_mib() {
    # MemAvailable is the kernel's own estimate of what can be handed out
    # without swapping — reclaimable cache included, which `free` column
    # arithmetic gets wrong.
    if [ -r /proc/meminfo ]; then
        awk '/^MemAvailable:/ {print int($2 / 1024); found = 1} END {if (!found) print 4096}' /proc/meminfo
    else
        echo 4096 # not Linux: assume little and let the caller override
    fi
}

clamp() { # clamp <value> <lo> <hi>
    local v=$1 lo=$2 hi=$3
    ((v < lo)) && v=$lo
    ((v > hi)) && v=$hi
    echo "$v"
}

if [ -n "${SC_TEST_MEM:-}" ]; then
    budget_mib=$(to_mib "$SC_TEST_MEM")
else
    budget_mib=$(clamp $(($(available_mib) * 6 / 10)) 4096 12288)
fi

cores=$( (nproc 2>/dev/null) || echo 4)

# ---------------------------------------------------------------------------
# The two dials.
# ---------------------------------------------------------------------------

# The build is close to linear in the job count — the two measurements above fit
# 1.2 GB of fixed cost plus ~0.45 GB a job — so solve that for the budget. The
# divisor here is twice the measured slope: what a cgroup accounts for is not
# only this anonymous memory but the page cache under the ~10 GB of artifacts a
# rebuild writes, and that cache is what turns into reclaim pressure.
jobs=${SC_TEST_JOBS:-$(clamp $(((budget_mib - 1229) / 900)) 1 "$cores")}

# The run phase is flat enough that the core count is usually the right answer;
# the dial exists for the machine where it is not, and for the database behind
# the suite, which gets a connection and a schema per thread.
threads=${SC_TEST_THREADS:-$(clamp $((budget_mib / 512)) 1 "$cores")}

# ---------------------------------------------------------------------------
# The run.
# ---------------------------------------------------------------------------

# The cgroup wall sits above the budget, not on it: MemoryHigh throttles into
# reclaim at the budget, and MemoryMax a little over it is the wall behind that.
export SC_BUILD_MEM_HIGH="${SC_BUILD_MEM_HIGH:-${budget_mib}M}"
export SC_BUILD_MEM_MAX="${SC_BUILD_MEM_MAX:-$((budget_mib + 2048))M}"

# A `-j` or a `--test-threads` the caller passed wins; this only fills gaps.
args=("$@")
printf '%s\0' "${args[@]+"${args[@]}"}" | grep -qz -- '-j\|--jobs' && jobs=""
printf '%s\0' "${args[@]+"${args[@]}"}" | grep -qz -- '--test-threads' && threads=""

echo "==> budget ${budget_mib} MiB: build -j${jobs:-(given)}, run --test-threads=${threads:-(given)}" >&2

build=("test" "${args[@]+"${args[@]}"}" "--no-run")
[ -n "$jobs" ] && build+=("-j" "$jobs")
"$here/cargo-guarded.sh" "${build[@]}"

run=("test" "${args[@]+"${args[@]}"}")
[ -n "$threads" ] && run+=("--" "--test-threads=$threads")
"$here/cargo-guarded.sh" "${run[@]}"
