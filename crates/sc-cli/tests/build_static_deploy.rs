//! `scripts/build-static.sh --deploy HOST`: the copy-unpack-install step that
//! runs after the tarball is written.
//!
//! The build itself is not what is under test here — compiling V8 to check a
//! `scp` would be absurd — so this drives the script inside a throwaway
//! "repository" (the script, and the `Cargo.toml` it reads the version from)
//! with a `PATH` in front of it holding stubs for the three commands the deploy
//! path cannot really run: `cargo` (writes a one-line executable where the build
//! would have left the binary), `rustup`, and `ssh`.
//!
//! The `ssh` stub is what makes this a real test rather than a spelling check:
//! it runs the command it was handed **locally**, so the remote half of the
//! deploy — the unpack, `install.sh`, the smoke run of the installed binary, the
//! cleanup — actually executes, against a `--prefix` and a `--remote-tmp` inside
//! the test's temporary directory. What the assertions then look at is the
//! installed tree, not the script's text.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && fs::read_to_string(&manifest)
                .unwrap_or_default()
                .contains("[workspace]")
        {
            return dir;
        }
        assert!(dir.pop(), "reached / without finding the workspace root");
    }
}

/// A unique scratch directory, removed by the caller on success (and left behind
/// on failure, where its contents are the evidence).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "feldspar-deploy-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn write_executable(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn deploy_copies_unpacks_and_installs_over_ssh() {
    let root = workspace_root();
    let dir = scratch("ok");

    // The repository the script thinks it is in: itself, plus the Cargo.toml it
    // reads the workspace version from. Not a git checkout, so the artifact name
    // is version-only and this test does not depend on the state of the tree.
    let repo = dir.join("repo");
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::copy(
        root.join("scripts/build-static.sh"),
        repo.join("scripts/build-static.sh"),
    )
    .expect("copy build-static.sh");
    fs::copy(root.join("Cargo.toml"), repo.join("Cargo.toml")).unwrap();

    let target = "x86_64-unknown-linux-gnu";
    let bin = dir.join("bin");
    let log = dir.join("ssh.log");

    // `cargo build --release --target T -p sc-cli`, minus the compiler: leave an
    // executable where the real build would have left one. It prints usage and
    // exits 0, which is what the deploy's smoke run checks for.
    write_executable(
        &bin.join("cargo"),
        &format!(
            "#!/bin/sh\nset -eu\nout=\"$PWD/target/{target}/release\"\n\
             mkdir -p \"$out\"\n\
             printf '#!/bin/sh\\necho feldspar usage\\n' > \"$out/feldspar\"\n\
             chmod +x \"$out/feldspar\"\n"
        ),
    );
    write_executable(
        &bin.join("rustup"),
        &format!("#!/bin/sh\n[ \"${{1:-}}\" = target ] && echo {target}\nexit 0\n"),
    );
    // The remote, played locally: record the argv, then run the command the
    // script sent (the last argument) with its stdin attached, exactly as the
    // far side of an ssh would.
    write_executable(
        &bin.join("ssh"),
        &format!(
            "#!/bin/sh\nset -eu\n\
             printf '%s\\n' \"$*\" >> {log}\n\
             for cmd; do :; done\n\
             exec sh -c \"$cmd\"\n",
            log = log.display()
        ),
    );
    // install.sh is run through sudo when the login is not root, which it is not
    // here; the prefix is writable, so the stub only has to get out of the way.
    write_executable(&bin.join("sudo"), "#!/bin/sh\nexec \"$@\"\n");

    // `--prefix`'s *parent* has to exist and be writable, or install.sh asks to
    // be re-run as root.
    let prefix = dir.join("opt/feldspar");
    fs::create_dir_all(prefix.parent().unwrap()).unwrap();
    let remote_tmp = dir.join("remote-tmp");

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new("bash")
        .arg(repo.join("scripts/build-static.sh"))
        .args(["--native", "--no-ui", "--no-strip", "--no-verify"])
        .arg("--prefix")
        .arg(&prefix)
        .arg("--remote-tmp")
        .arg(&remote_tmp)
        .arg("--output")
        .arg(dir.join("dist"))
        .args(["--ssh-opt", "-oStrictHostKeyChecking=no", "--deploy", "vm"])
        .env("PATH", &path)
        .output()
        .expect("run build-static.sh");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "build-static.sh --deploy failed ({})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status
    );

    // 1. It installed: the tree is at the prefix on the "remote", and the binary
    //    is executable there.
    let installed = prefix.join("bin/feldspar");
    assert!(
        installed.is_file(),
        "nothing was installed at {installed:?}\n{stdout}\n{stderr}"
    );
    assert!(
        fs::metadata(&installed).unwrap().permissions().mode() & 0o111 != 0,
        "the installed binary is not executable"
    );

    // 2. It cleaned up after itself: the tarball, the unpacked tree and the
    //    generated remote script are all gone from the staging directory.
    let leftovers: Vec<String> = fs::read_dir(&remote_tmp)
        .expect("the remote staging directory should still exist")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "the deploy left {leftovers:?} in {remote_tmp:?}"
    );

    // 3. The artifact is still written locally — a deploy is in addition to the
    //    tarball, not instead of it.
    let tarballs: Vec<String> = fs::read_dir(dir.join("dist"))
        .expect("dist")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tar.gz"))
        .collect();
    assert_eq!(tarballs.len(), 1, "expected one tarball, got {tarballs:?}");

    // 4. What went over the wire, in order: the reachability probe before the
    //    build, the tarball, the remote script, and the run of that script. And
    //    --ssh-opt reached every one of them.
    let sent = fs::read_to_string(&log).expect("the ssh stub should have logged");
    let calls: Vec<&str> = sent.lines().collect();
    assert!(
        calls.len() >= 4,
        "expected a probe, two copies and a run, got {calls:?}"
    );
    assert!(
        calls[0].contains("BatchMode=yes"),
        "the first ssh should be the pre-build reachability probe: {}",
        calls[0]
    );
    assert!(
        calls.iter().any(|c| c.contains(".tar.gz")),
        "the tarball was never sent: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("sh ") && c.contains(".deploy.sh")),
        "the remote script was never run: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|c| c.contains("-oStrictHostKeyChecking=no")),
        "--ssh-opt must be passed to every ssh invocation: {calls:?}"
    );
    assert!(
        stdout.contains("installed") && stdout.contains("vm"),
        "the script should report where it installed:\n{stdout}"
    );

    fs::remove_dir_all(&dir).ok();
}

/// The options that cannot work are refused before the build, not after an hour
/// of it: a `--deploy` with no host, and a relative `--remote-tmp`.
#[test]
fn unusable_deploy_options_are_refused_up_front() {
    let script = workspace_root().join("scripts/build-static.sh");

    let missing_value = Command::new("bash")
        .arg(&script)
        .arg("--deploy")
        .output()
        .expect("run build-static.sh");
    assert_eq!(missing_value.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&missing_value.stderr).contains("--deploy needs a value"),
        "a --deploy with no host should say so"
    );

    let relative_tmp = Command::new("bash")
        .arg(&script)
        .args(["--deploy", "vm", "--remote-tmp", "tmp"])
        .output()
        .expect("run build-static.sh");
    assert_eq!(relative_tmp.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&relative_tmp.stderr).contains("--remote-tmp must be absolute"),
        "a relative --remote-tmp should be refused"
    );

    // And the deploy options are documented, since --help is where an operator
    // finds them.
    let help = Command::new("bash")
        .arg(&script)
        .arg("--help")
        .output()
        .expect("run build-static.sh --help");
    let help = String::from_utf8_lossy(&help.stdout);
    for option in ["--deploy", "--ssh-opt", "--remote-tmp"] {
        assert!(help.contains(option), "--help should document {option}");
    }
}
