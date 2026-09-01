//! This process's name resolution, with glibc's NSS taken out of the path
//! (layer 0).
//!
//! # The failure this exists to prevent
//!
//! `scripts/build-static.sh` links the binary with `+crt-static`: one file, no
//! interpreter, no shared-library dependencies. glibc's `getaddrinfo` does not
//! respect that. It reads `/etc/nsswitch.conf` and **`dlopen`s a shared object
//! per module named on the `hosts:` line** — `libnss_myhostname.so.2`,
//! `libnss_mdns4_minimal.so.2`, `libnss_systemd.so.2`. Each of those is linked
//! against the *shared* glibc, so loading one drags `ld-linux.so` and a second,
//! complete `libc.so.6` into a process that already has one statically linked.
//! Two glibcs, one process: the second one runs with state the first one owns.
//!
//! The observed result on a Debian VM was a `SIGFPE` inside the dynamically
//! loaded `libc.so.6`, a second after `READY=1`, restarting forever. It appeared
//! only once TLS was switched on, because ACME's call to the CA is the first
//! hostname this server ever resolves — the database is a Unix socket, and
//! nothing else on the boot path has a name to look up. `files` and `dns` are
//! built into glibc 2.34+ and load nothing, which is why the artifact tested
//! clean on Alpine and on a workstation whose `hosts:` line happens to be
//! ordinary.
//!
//! # What this does instead
//!
//! The linker is told `--wrap=getaddrinfo` (see `build.rs`), so every
//! unresolved reference to `getaddrinfo` in the binary — `std`'s
//! `ToSocketAddrs`, and therefore tokio, `async-net`, `reqwest`,
//! `tokio-postgres`, `lettre` and Deno's `fetch` alike — lands in
//! [`__wrap_getaddrinfo`](addrinfo::__wrap_getaddrinfo) instead. Hostnames are
//! resolved by [`hickory_resolver`] over `/etc/resolv.conf` and `/etc/hosts`,
//! in Rust, with no `dlopen` anywhere. The answer is marshalled back into the
//! `addrinfo` list the C ABI promises, and freed again by
//! [`__wrap_freeaddrinfo`](addrinfo::__wrap_freeaddrinfo).
//!
//! **One place, not one per client.** The alternative was a resolver setting per
//! HTTP client — a feature flag on `reqwest`, a replacement for `rustls-acme`,
//! and no answer at all for an application's own `fetch()`, which resolves
//! through Deno. Any single client left on `getaddrinfo` re-arms the crash, so
//! the interception belongs at the one place they all pass through.
//!
//! # What still goes to glibc
//!
//! Three calls are delegated to `__real_getaddrinfo`, and none of them consults
//! the `hosts:` line, so none of them loads an NSS module:
//!
//! - a **null `node`** — the service-only lookup a caller makes to bind a port;
//! - a **named service** (`"https"` rather than `"443"`), which is `/etc/services`
//!   and is resolved by a second, host-less call;
//! - nothing else.
//!
//! # What this deliberately does not do
//!
//! - **`gethostbyname`, `getnameinfo`, `getaddrinfo_a`** are not wrapped.
//!   Nothing in this tree calls them; a dependency that started to would be back
//!   on NSS.
//! - **NSS host modules stop applying to this process.** No mDNS (`.local`), no
//!   `myhostname` synthesis of the local hostname, no LDAP/sssd hosts. A
//!   deployment that resolves names through any of those must put them in DNS or
//!   in `/etc/hosts`.
//! - **RFC 6724 address sorting** is not implemented. Addresses come back IPv4
//!   first, then IPv6 (`LookupIpStrategy::Ipv4AndIpv6`), which is what every
//!   caller in this tree wants: they walk the list until one connects.

pub mod addrinfo;
mod resolver;

pub use addrinfo::intercepted;
pub use resolver::init;
