//! The git file-store backend, against a **real** repository.
//!
//! The remote is a bare repository in a temp directory rather than a hosted one:
//! `git clone`, `pull` and `push` behave identically over a local path, so this
//! exercises the whole round trip — clone, edit through the `FileStore` API,
//! commit, push, and see the push arrive — with no network and no credentials.
//! What it deliberately does not cover is SSH authentication, which cannot be
//! tested without a remote to authenticate to; the deploy-key test below checks
//! the key is generated in the right shape and with the right permissions, which
//! is the half that is ours rather than OpenSSH's.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use bytes::Bytes;
use sc_files::{
    CFG_BRANCH, DATA_DIR_ENV, FileStore, FileStoreDef, GIT_BACKEND, GitFileStore, GitRepo,
    clone_path, connect_from_def, generate_deploy_key, record_clone_path,
    validate_file_store_config,
};

/// A fresh unique temp directory for one test.
fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sc-files-git-{tag}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run git in `dir`, asserting it succeeded.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare repository with one commit on `main`, standing in for the remote an
/// admin would paste the URL of.
fn origin_with_a_commit(tag: &str) -> PathBuf {
    let base = temp_dir(tag);
    let bare = base.join("origin.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "--initial-branch=main", "."]);

    // Seed it through a throwaway working copy: a bare repo has no tree to
    // commit in.
    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "--initial-branch=main", "."]);
    git(&seed, &["config", "user.email", "seed@example.com"]);
    git(&seed, &["config", "user.name", "Seed"]);
    std::fs::write(seed.join("README.md"), "# from the remote\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&seed, &["push", "origin", "main"]);

    bare
}

/// A git store definition pointed at `url`, with its clone directory pinned into
/// a temp directory — never the developer's real data directory.
fn git_store_def(name: &str, url: &Path, clone_into: &Path) -> FileStoreDef {
    let mut def = FileStoreDef::git(name, url.to_string_lossy().into_owned());
    record_clone_path(&mut def, clone_into);
    def
}

#[tokio::test]
async fn a_git_store_clones_serves_commits_and_pushes() {
    let origin = origin_with_a_commit("roundtrip");
    let workspace = temp_dir("roundtrip-clone");
    let def = git_store_def("app", &origin, &workspace.join("app"));

    // The definition is well-formed …
    validate_file_store_config(&def).unwrap();
    let repo = GitRepo::from_def(&def).unwrap();
    // … but nothing has been cloned, so there is nothing to connect to yet.
    assert!(!repo.is_cloned());
    assert!(connect_from_def(&def).is_err());
    assert!(!repo.status().await.unwrap().cloned);

    // Clone. This is the explicit, awaited step — connecting never does it.
    repo.ensure_cloned().await.unwrap();
    assert!(repo.is_cloned());

    // And now it is an ordinary file store, serving what the remote had.
    let store = connect_from_def(&def).unwrap();
    assert_eq!(store.name(), "app");
    assert!(store.is_git_repo());
    let readme = store.read("README.md").await.unwrap();
    assert_eq!(&readme[..], b"# from the remote\n");

    // A second clone is a no-op rather than a re-clone: saving the store must
    // not throw away a working tree.
    let again = repo.ensure_cloned().await.unwrap();
    assert!(again.success);
    assert!(again.output.contains("already cloned"), "{}", again.output);

    // Write through the FileStore API — the admin's file manager or an app's
    // build output — and git sees it.
    store
        .write("notes/today.md", Bytes::from_static(b"hello\n"))
        .await
        .unwrap();
    let status = repo.status().await.unwrap();
    assert!(status.cloned);
    assert_eq!(status.branch, "main");
    assert!(!status.clean(), "the new file should show as a change");
    assert!(
        status.changes.iter().any(|c| c.contains("notes/")),
        "{:?}",
        status.changes
    );
    assert!(status.last_commit.contains("initial"), "{}", status.last_commit);

    // Commit everything, with a message.
    let commit = repo.commit_all("add today's notes").await.unwrap();
    assert!(commit.committed, "{}", commit.output);
    let status = repo.status().await.unwrap();
    assert!(status.clean());
    assert!(status.last_commit.contains("add today's notes"));
    // Committed but not pushed: one commit ahead of the remote.
    assert_eq!((status.ahead, status.behind), (1, 0));

    // Committing again with nothing changed is a no-op, not a failure.
    let nothing = repo.commit_all("nothing to see").await.unwrap();
    assert!(!nothing.committed, "{}", nothing.output);

    // Push, and the remote has it.
    repo.push().await.unwrap();
    assert_eq!(
        git(&origin, &["show", "main:notes/today.md"]),
        "hello"
    );
    let status = repo.status().await.unwrap();
    assert_eq!((status.ahead, status.behind), (0, 0));
}

#[tokio::test]
async fn a_pull_brings_down_what_someone_else_pushed() {
    let origin = origin_with_a_commit("pull");
    let workspace = temp_dir("pull-clone");
    let def = git_store_def("app", &origin, &workspace.join("app"));
    let repo = GitRepo::from_def(&def).unwrap();
    repo.ensure_cloned().await.unwrap();
    // A store cloned by us commits with our fallback identity when the machine
    // has none configured; make the test independent of the developer's config.
    let store = GitFileStore::new("app", GitRepo::from_def(&def).unwrap()).unwrap();

    // Someone else pushes a file to the remote.
    let other = workspace.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["clone", &origin.to_string_lossy(), "."]);
    git(&other, &["config", "user.email", "other@example.com"]);
    git(&other, &["config", "user.name", "Other"]);
    std::fs::write(other.join("upstream.txt"), "from elsewhere\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-m", "upstream change"]);
    git(&other, &["push", "origin", "main"]);

    // Before the pull the store cannot see it …
    assert!(store.read("upstream.txt").await.is_err());
    // … after it, it can, and the file store serves it.
    repo.pull().await.unwrap();
    let got = store.read("upstream.txt").await.unwrap();
    assert_eq!(&got[..], b"from elsewhere\n");
    assert!(repo.status().await.unwrap().clean());
}

#[tokio::test]
async fn a_branch_setting_checks_out_that_branch() {
    let origin = origin_with_a_commit("branch");
    // Add a second branch to the remote.
    let workspace = temp_dir("branch-clone");
    let seed = workspace.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["clone", &origin.to_string_lossy(), "."]);
    git(&seed, &["config", "user.email", "seed@example.com"]);
    git(&seed, &["config", "user.name", "Seed"]);
    git(&seed, &["checkout", "-b", "release"]);
    std::fs::write(seed.join("VERSION"), "1.0\n").unwrap();
    git(&seed, &["add", "-A"]);
    git(&seed, &["commit", "-m", "release 1.0"]);
    git(&seed, &["push", "origin", "release"]);

    let mut def = FileStoreDef::git("app", origin.to_string_lossy().into_owned())
        .with(CFG_BRANCH, "release");
    record_clone_path(&mut def, &workspace.join("app"));
    let repo = GitRepo::from_def(&def).unwrap();
    repo.ensure_cloned().await.unwrap();

    let status = repo.status().await.unwrap();
    assert_eq!(status.branch, "release");
    let store = connect_from_def(&def).unwrap();
    assert_eq!(&store.read("VERSION").await.unwrap()[..], b"1.0\n");
}

#[tokio::test]
async fn a_bad_url_fails_the_clone_with_gits_own_message() {
    let workspace = temp_dir("badurl");
    let def = git_store_def(
        "app",
        &workspace.join("no-such-repository"),
        &workspace.join("app"),
    );
    let repo = GitRepo::from_def(&def).unwrap();
    let err = repo.ensure_cloned().await.unwrap_err().to_string();
    // The admin needs git's reason, not "clone failed".
    assert!(err.contains("no-such-repository"), "{err}");
    assert!(!repo.is_cloned());
}

#[tokio::test]
async fn operations_on_a_store_that_was_never_cloned_say_so() {
    let workspace = temp_dir("uncloned");
    let def = git_store_def(
        "app",
        Path::new("git@example.com:me/app.git"),
        &workspace.join("app"),
    );
    let repo = GitRepo::from_def(&def).unwrap();

    for err in [
        repo.pull().await.unwrap_err().to_string(),
        repo.push().await.unwrap_err().to_string(),
        repo.commit_all("anything").await.unwrap_err().to_string(),
    ] {
        assert!(err.contains("not a git clone yet"), "{err}");
    }
    // `status` is the exception: asking what state an unconnected store is in is
    // exactly how the UI decides to offer a Clone button, so it answers rather
    // than failing.
    assert!(!repo.status().await.unwrap().cloned);
}

#[tokio::test]
async fn an_empty_commit_message_is_refused() {
    let origin = origin_with_a_commit("nomsg");
    let workspace = temp_dir("nomsg-clone");
    let def = git_store_def("app", &origin, &workspace.join("app"));
    let repo = GitRepo::from_def(&def).unwrap();
    repo.ensure_cloned().await.unwrap();

    let err = repo.commit_all("   ").await.unwrap_err().to_string();
    assert!(err.contains("message"), "{err}");
}

#[tokio::test]
async fn a_generated_deploy_key_is_an_ed25519_key_only_we_can_read() {
    let data = temp_dir("deploykey");
    // Scoped to this test's temp directory, so nothing lands in the real one.
    let key = with_data_dir(&data, || generate_deploy_key("my app"))
        .await
        .unwrap();

    // The public half is what the admin pastes into the repository's settings.
    assert!(
        key.public_key.starts_with("ssh-ed25519 "),
        "{}",
        key.public_key
    );
    assert!(key.public_key.contains("saltcorn:my app"), "{}", key.public_key);
    assert!(!key.public_key.contains('\n'), "one line, to be pasted");

    // The private half stays on disk, under the name-derived filename, and is
    // readable only by us — `ssh` refuses a key that is not.
    assert_eq!(
        key.private_key_path,
        data.join("keys").join("my_app.key"),
        "the key is named after the store, made path-safe"
    );
    let private = std::fs::read_to_string(&key.private_key_path).unwrap();
    assert!(private.contains("OPENSSH PRIVATE KEY"), "{private}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key.private_key_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    // Generating again replaces it: an admin who needs a new key must be able to
    // get one, and the old public key may already have been rejected.
    let again = with_data_dir(&data, || generate_deploy_key("my app"))
        .await
        .unwrap();
    assert_eq!(again.private_key_path, key.private_key_path);
    assert_ne!(again.public_key, key.public_key);
}

#[tokio::test]
async fn a_store_the_admin_has_not_cloned_still_knows_where_it_would_go() {
    // `clone_path` is what the whole flow hangs off, so it must answer for a
    // definition that has never been saved or cloned.
    let data = temp_dir("clonepath");
    let def = FileStoreDef::git("web app", "git@example.com:me/app.git");
    let path = with_data_dir(&data, || async { clone_path(&def).unwrap() }).await;
    assert_eq!(path, data.join("git-stores").join("web_app"));
    assert_eq!(def.backend, GIT_BACKEND);
}

/// Run an operation with [`DATA_DIR_ENV`] pointed at `dir`.
///
/// The variable is process-wide, so this serialises on **one** lock and restores
/// what was there — two tests setting it concurrently would each see the other's
/// directory, and the failure would be a key or a clone appearing in the wrong
/// place, which is the kind of thing that only fails sometimes. That is also why
/// there is a single helper rather than a sync and an async one: two helpers
/// meant two locks, which serialised nothing.
async fn with_data_dir<T, F, Fut>(dir: &Path, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    // A tokio mutex rather than a `std` one: it is held across the await, and it
    // has to be — the environment must not change while the operation runs.
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let before = set_data_dir(Some(dir));
    let out = f().await;
    set_data_dir(before.as_deref().map(Path::new));
    out
}

/// Set (or clear) the data-directory variable, returning what it was.
fn set_data_dir(dir: Option<&Path>) -> Option<String> {
    let before = std::env::var(DATA_DIR_ENV).ok();
    unsafe {
        match dir {
            Some(d) => std::env::set_var(DATA_DIR_ENV, d),
            None => std::env::remove_var(DATA_DIR_ENV),
        }
    }
    before
}
