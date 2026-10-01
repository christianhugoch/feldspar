# Building feldspar natively on Alpine (musl)

How to run `cargo build --release -p sc-cli` on an Alpine Linux machine: musl libc, no glibc.
This was first worked out on an ARM tablet (Honor MagicPad 4) running Alpine 3.21 inside Acode
(a proot environment), aarch64, rustc 1.99.

**Short version:**

```sh
deploy/alpine-musl/build.sh            # installs packages, prepares V8, patches, builds
# or, once setup.sh has run:
. deploy/alpine-musl/env.sh && cargo build --release -p sc-cli
```

The output is `target/release/feldspar`, a dynamically linked musl binary.

If what you want is a binary *to deploy* on Alpine, you probably don't need any of this.
`scripts/build-static.sh` builds a static glibc binary on a glibc machine (or in docker), and that
binary runs on Alpine unchanged. This document is for when the *build machine itself* is Alpine.

## The problem

`deno_core` links V8 through the `v8` crate (rusty_v8). That crate's `build.rs` downloads a
prebuilt static archive from the rusty_v8 GitHub releases. Upstream publishes archives for
`*-linux-gnu`, macOS and Windows only, so on musl the download 404s:

```
static lib URL: https://github.com/denoland/rusty_v8/releases/download/v149.4.0/librusty_v8_simdutf_release_aarch64-unknown-linux-musl.a.gz
HTTP Error 404: Not Found
```

The alternatives considered and rejected:

| Approach | Why not |
|---|---|
| `V8_FROM_SOURCE=1` | Hours of GN/ninja per V8 bump. It needs a newer clang than Alpine stable ships, and musl is not a supported V8 configuration. |
| Cross-compile only the final binary to `aarch64-unknown-linux-gnu` against a Debian sysroot | Not enough. `sc-module`'s **build script** runs V8 on the build host to make the `deno_runtime` snapshot, so V8 must also link and run as a *host* (musl) binary. |
| A Debian chroot (works under proot) | Works, but means a second distro, a second Rust toolchain and a second cargo cache. Kept as the fallback if the shim approach ever stops working. |

## The approach: glibc V8 archive and a shim

The prebuilt **glibc** archive is position-independent C++ with its own bundled libc++. What ties
it to glibc is the set of libc symbols it imports. Of its undefined symbols, roughly 30 exist in
glibc but not in musl. The rest come from Rust, compiler-builtins or libunwind. The 30 are:

- the LFS `*64` aliases (`open64`, `mmap64`, `fopen64`, `readdir64`, …). musl ≥ 1.2.4 removed
  them, and on 64-bit they are the same as the plain calls.
- `__xstat64` / `__lxstat64` / `__fxstat64`, the old glibc stat entry points.
- the `_FORTIFY_SOURCE` checked variants (`__memcpy_chk`, `__vsnprintf_chk`, …).
- `backtrace*`, from glibc's execinfo. These are only used for V8 crash dumps.
- `__libc_stack_end` and `__cxa_thread_atexit_impl`.

`deploy/alpine-musl/glibc_shim.c` implements those on top of musl. `setup.sh` compiles it and
**appends the object to a copy of the archive**. The linker then pulls it in exactly when V8 needs
it, with no extra linker flags. `env.sh` points the v8 crate at that copy through its own escape
hatches, `RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH`. The generated bindings are the gnu
ones, which is correct: the ABI and struct layouts are identical.

To check what's missing after a V8 bump, compare the archive's undefined symbols with what it
defines and what musl's `libc.a` defines:

```sh
nm=llvm-nm; a=librusty_v8_gnu.a
$nm -u -j $a | grep -v ':$' | sort -u > undef
$nm --defined-only -j $a | grep -v ':$' | sort -u > def
$nm --defined-only -j /usr/lib/libc.a | grep -v ':$' | sort -u > musl
comm -23 undef def | comm -23 - musl | grep -v '^temporal_rs_\|^v8_\|^rusty_v8_\|^crdtp_\|^__aarch64_\|^_Unwind_'
```

Anything left that isn't a compiler-rt name (`__addtf3`, `__udivti3`, …) belongs in the shim.

## The other things that break on musl

These came up one at a time, each after the previous one was fixed. All of them are handled by
the scripts.

1. **`bindgen` can't load libclang:** "Unable to find libclang … Dynamic loading not supported".
   Rust's musl target links statically by default, and a static build script can't `dlopen`.
   `env.sh` sets `RUSTFLAGS=-C target-feature=-crt-static`, which is the standard Alpine fix. It
   also sets `LIBCLANG_PATH`. The resulting binary links against Alpine's musl dynamically.

2. **`libffi-sys`: `linux/limits.h: No such file`.** Install `linux-headers`.

3. **`deno_runtime` does not compile:** "cannot find function `malloc_trim` in crate `libc`".
   `malloc_trim` is glibc-only, and deno calls it unconditionally on Linux.
   `patch-registry.sh` gates the two calls on `target_env = "gnu"`. On musl the memory-release
   hint is simply skipped.

4. **`sc-module`'s build script dies with SIGSEGV** before `main`. The cause is `deno_node`
   (`ops/process.rs`), which registers an `.init_array` constructor taking
   `(argc, argv, envp)`. glibc passes those arguments to constructors; musl passes nothing, so
   the hook dereferences garbage. The trail: gdb showed a crash inside `do_init_fini`, in
   opt-level-0 Rust code computing `argv[argc-1] + strlen + 1`. Grepping the registry for
   `.init_array` together with `argc` found it. `patch-registry.sh` gates the hook on
   `target_env = "gnu"`. As a result, Node's `process.title = …` is a no-op on musl.

**Patches 3 and 4 edit `~/.cargo/registry/src`, not the repository.** Cargo does not notice
edits to registry sources, so a crate compiled before the patch stays stale. `setup.sh` runs
`cargo clean -p` on each crate the patch script reports as changed. Re-run `setup.sh` (or just
`build.sh`) after `cargo update`, after a deno bump, or after clearing the cargo cache. A deno
bump that moves the code will make the patch script silently find nothing to patch, and the
symptoms above come back. Update the patterns in `patch-registry.sh` if that happens.

## Files

| File | Purpose |
|---|---|
| `deploy/alpine-musl/build.sh` | `setup.sh` + `env.sh` + `cargo build --release -p sc-cli "$@"` |
| `deploy/alpine-musl/setup.sh` | apk packages, V8 archive + shim, registry patches. Idempotent. `--no-apk` skips the package install. |
| `deploy/alpine-musl/env.sh` | Source before running cargo yourself. It reads the V8 version from `Cargo.lock`. |
| `deploy/alpine-musl/glibc_shim.c` | The glibc-only symbols, on top of musl |
| `deploy/alpine-musl/patch-registry.sh` | The `deno_runtime` / `deno_node` source patches |

Prepared archives live in `~/.local/share/feldspar-musl-v8/<v8 version>/`. Override the location
with `SC_MUSL_V8_DIR`. The archive flavour defaults to `simdutf_release`, matching the v8 crate's
default features; override it with `SC_V8_FLAVOUR`.

## Practical notes for small machines

- **Memory.** `.cargo/config.toml` caps the build at 4 jobs. On the tablet, a build at 4 jobs was
  killed partway through. The build environment died, and cargo then reported
  `Function not implemented (os error 38)` when spawning rustc. `build.sh -j 2` completed.
  Run the build under `nohup … &` so it survives the terminal or the agent session going away.
- **UI bundles.** `sc-cli`'s `build.rs` runs `npm ci && npm run build` for four UI bundles by
  default. Set `SC_BUILD_ADMIN=0` to skip them if Node isn't available.
- **x86_64 Alpine.** The scripts select the x86_64 archive on x86_64, and the shim is written
  for any 64-bit Linux. Only aarch64 has actually been built and run.

## Caveats

- This is not a configuration upstream supports. V8 compiled against glibc runs on musl through
  the shim. The snapshot step does exercise V8 at build time (it creates an isolate, runs the
  deno bootstrap JS and serialises a heap), which is a reasonable smoke test. Even so, run the
  JS-heavy test suites before trusting a musl build for anything serious.
- V8 crash backtraces are empty, because the shim's `backtrace()` returns 0.
- `__libc_stack_end` is approximated from a constructor's frame address, close to the top of
  the main thread's stack.
- `thread_local` destructors registered through `__cxa_thread_atexit_impl` run at thread exit
  through a pthread key. On the main thread they don't run at process exit, whereas glibc would
  run them.
