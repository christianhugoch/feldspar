# Saltcorn v2 — Build hosts (milestones B1–B4)

Building an application's native app on another machine. The server stays on Linux; a Mac
registered as a build host builds the iOS targets for it. The design is
[docs/TECHNICAL_DESIGN.md](./docs/TECHNICAL_DESIGN.md) §13.3, "Build hosts". Milestones are
prefixed **B**; work through them in order.

The rule everything else follows: **the build host never generates code, it only builds.** The
server writes the generated client and runtime from its own catalog before it hands a job out,
and the generated app names tables, never their ids, so a build host needs no database and
nothing on it can drift from the server's schema.

Legend: `[ ]` todo · `[~]` in progress · `[x]` done.

## B1 — Registering a build host

- [ ] B1.1 `_fd_build_hosts`: a name, the targets it builds (`ios`, `ios_simulator`, later
      `android`), the hash of its token, when it was last seen, and what it last reported (OS,
      `feldspar` version, Xcode version, readiness per target).
- [ ] B1.2 Settings → Build hosts: add a host (its token is shown once), rename it, choose its
      targets, revoke it. The list shows whether each host is online.
- [ ] B1.3 `feldspar build-agent --server <url> --token <token>`: the same binary, started on the
      Mac. It needs no database. It checks the targets' requirements on its own machine and
      sends a heartbeat with what it found.
- [ ] B1.4 `/build-agent/*` routes that take the build host's bearer token and nothing else. A
      build host token opens no other endpoint, and no other token opens these.
- [ ] B1.5 An agent whose `feldspar` version is too old for the server is refused with a message
      saying which version it needs.

## B2 — Jobs

- [ ] B2.1 `_fd_build_jobs`: a queued target build, with the application, the target, the build
      host that took it, its state and its lease. Jobs survive a restart, unlike today's jobs,
      which live in memory.
- [ ] B2.2 The build button: when the target's requirements do not hold on the server but an
      online build host builds it, the job is queued instead of run here.
- [ ] B2.3 Before queueing, the server regenerates the client and runtime as for a local build,
      then packs the project as a `tar.gz`. It leaves out what the backup leaves out
      (`node_modules`) and what the build makes (`android`, `ios`, `*-output`, `build-logs`).
      Android signing keys (`*.p12`, `*.jks`) stay out of iOS jobs.
- [ ] B2.4 The agent claims a job by long-polling. The answer is the archive and the rendered
      target: command, arguments, environment (the iOS secrets included) and artifact path.
- [ ] B2.5 The agent unpacks into a workspace it keeps per application, runs the install step and
      the command, and checks that the artifact is there. It reuses the local build's code.
- [ ] B2.6 The log goes back in chunks while the build runs. The server writes it to
      `build-logs/` as a local build does, so the admin can follow it.
- [ ] B2.7 The artifact is uploaded and written into the store at the usual path
      (`ios-output/app.ipa`), so it is downloaded from Files as before.
- [ ] B2.8 A heartbeat renews the lease while the job runs. A job whose lease runs out fails with
      a reason saying the build host stopped answering.
- [ ] B2.9 Tests: a fake agent in the test suite claims a job, sends a log and an artifact, and the
      file is in the store; a lost agent fails its job; a wrong token opens nothing.

## B3 — Readiness and control

- [ ] B3.1 Readiness says where a target builds: here, on a named build host, or nowhere (with the
      reasons from both). The button no longer warns "needs macOS" when a Mac is registered.
- [ ] B3.2 Cancelling a running job: the agent hears it in its next heartbeat answer and stops the
      build.
- [ ] B3.3 A timeout per job, so a build that hangs fails instead of holding the target forever.
- [ ] B3.4 The agent's workspace keeps `node_modules` and the CocoaPods between builds, and only
      replaces the project's own files.

## B4 — Documentation

- [ ] B4.1 OPERATIONS: setting up a Mac as a build host (Xcode, CocoaPods, the distribution
      certificate in the keychain, running the agent as a service).
- [ ] B4.2 TECHNICAL_DESIGN §13.3 brought up to date.

## Later

- [ ] Android on a build host too, for a server without the Android SDK.
- [ ] Choosing between several build hosts for one target (least busy, or the one the
      application names).
- [ ] Encrypting the build secrets at rest. The App Store Connect key and the keystore password are
      plain JSON in the database today, masked only on the wire.
