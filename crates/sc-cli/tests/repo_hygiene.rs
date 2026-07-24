//! Guards the Phase 0 "repo hygiene" artifacts so they cannot silently
//! disappear or lose their required gates. This does not run rustfmt/clippy
//! themselves (those toolchain components may be absent locally and are
//! exercised in CI); it asserts the configuration that drives them exists and
//! declares the pieces the workspace depends on.

use std::fs;
use std::path::{Path, PathBuf};

/// Walk up from this crate's manifest dir to the workspace root (the ancestor
/// whose `Cargo.toml` declares `[workspace]`).
fn workspace_root() -> PathBuf {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            let contents = fs::read_to_string(&manifest).unwrap_or_default();
            if contents.contains("[workspace]") {
                return dir;
            }
        }
        assert!(
            dir.pop(),
            "reached the filesystem root without finding a [workspace] Cargo.toml"
        );
    }
}

fn read(root: &Path, rel: &str) -> String {
    let path = root.join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing {rel}: {e}"))
}

#[test]
fn rustfmt_config_present_and_pins_edition() {
    let root = workspace_root();
    let cfg = read(&root, "rustfmt.toml");
    // Edition must be pinned so `cargo fmt` on a stable toolchain matches the
    // 2024-edition workspace.
    assert!(cfg.contains("edition"), "rustfmt.toml must pin an edition");
    assert!(cfg.contains("2024"), "rustfmt.toml edition should be 2024");
}

#[test]
fn clippy_config_exempts_tests_from_unwrap_lints() {
    let root = workspace_root();
    let cfg = read(&root, "clippy.toml");
    assert!(cfg.contains("allow-unwrap-in-tests"));
    assert!(cfg.contains("allow-expect-in-tests"));
}

#[test]
fn ci_workflow_runs_fmt_clippy_and_test() {
    let root = workspace_root();
    let ci = read(&root, ".github/workflows/ci.yml");
    // The three required gates from the TODO must be wired.
    assert!(ci.contains("cargo fmt"), "CI must gate on rustfmt");
    assert!(ci.contains("cargo clippy"), "CI must gate on clippy");
    assert!(ci.contains("cargo test"), "CI must gate on tests");
}

#[test]
fn gitignore_covers_rust_and_node() {
    let root = workspace_root();
    let ignore = read(&root, ".gitignore");
    // Rust build output.
    assert!(ignore.contains("target"), ".gitignore must ignore target/");
    // Node / frontend output for the React apps served by sc-server.
    assert!(
        ignore.contains("node_modules"),
        ".gitignore must ignore node_modules/"
    );
}

/// The markdown documents the documentation set consists of: the top-level
/// entry points plus everything in `docs/`.
fn documentation_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = ["README.md", "TODO.md", "CLAUDE.md"]
        .iter()
        .map(|rel| root.join(rel))
        .filter(|p| p.is_file())
        .collect();
    let docs = root.join("docs");
    let entries = fs::read_dir(&docs).unwrap_or_else(|e| panic!("missing docs/: {e}"));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    assert!(
        out.iter().any(|p| p.ends_with("docs/TECHNICAL_DESIGN.md")),
        "the technical design document must be part of the documentation set"
    );
    out
}

/// Every markdown link to a `.md` file, in every document, must resolve to a
/// file that exists — a tutorial that links a sibling that was renamed or never
/// written is a broken promise the reader discovers instead of a test.
///
/// Only `.md` targets are checked (external URLs, anchors and code snippets that
/// merely look like links are left alone), which is exactly the class of links
/// the docs use to refer to each other.
#[test]
fn documentation_links_resolve() {
    let root = workspace_root();
    for doc in documentation_files(&root) {
        let text = fs::read_to_string(&doc).unwrap_or_else(|e| panic!("{doc:?}: {e}"));
        let dir = doc.parent().unwrap_or(&root);
        // Markdown inline links: `[label](target)`. Scan for `](` and take the
        // target up to the closing parenthesis.
        for (idx, _) in text.match_indices("](") {
            let rest = &text[idx + 2..];
            let Some(end) = rest.find(')') else { continue };
            let target = &rest[..end];
            // Strip a `#fragment`; skip external URLs and non-.md targets.
            let path_part = target.split('#').next().unwrap_or("");
            if path_part.contains("://") || !path_part.ends_with(".md") {
                continue;
            }
            let resolved = dir.join(path_part);
            assert!(
                resolved.is_file(),
                "{}: broken link `{target}` (resolved to {resolved:?})",
                doc.display()
            );
        }
    }
}

/// The tutorials link to each other, so a reader who finishes one finds the
/// next: the React tutorial leads to the file-fields tutorial, which leads back
/// and on to the ownership tutorial, which builds on it.
#[test]
fn tutorials_are_cross_linked() {
    let root = workspace_root();
    let react = read(&root, "docs/tutorial-react-todo.md");
    assert!(
        react.contains("tutorial-file-fields.md"),
        "the React tutorial should point at the file-fields tutorial as a next step"
    );
    let files = read(&root, "docs/tutorial-file-fields.md");
    assert!(
        files.contains("tutorial-react-todo.md"),
        "the file-fields tutorial builds on the React tutorial and should link it"
    );
    assert!(
        files.contains("tutorial-ownership.md"),
        "the file-fields tutorial should point at the ownership tutorial as a next step"
    );
    let ownership = read(&root, "docs/tutorial-ownership.md");
    assert!(
        ownership.contains("tutorial-file-fields.md"),
        "the ownership tutorial builds on the file-fields tutorial and should link it"
    );
}
