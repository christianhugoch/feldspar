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
    assert!(
        ownership.contains("tutorial-triggers.md"),
        "the ownership tutorial should point at the triggers tutorial as a next step"
    );
    let triggers = read(&root, "docs/tutorial-triggers.md");
    assert!(
        triggers.contains("tutorial-ownership.md"),
        "the triggers tutorial builds on the ownership tutorial and should link it"
    );
    assert!(
        triggers.contains("tutorial-agents.md"),
        "the triggers tutorial should point at the agents tutorial as a next step"
    );
    let agents = read(&root, "docs/tutorial-agents.md");
    assert!(
        agents.contains("tutorial-triggers.md"),
        "the agents tutorial builds on the triggers tutorial and should link it"
    );
    assert!(
        agents.contains("tutorial-graphql.md"),
        "the agents tutorial should point at the GraphQL tutorial as a next step"
    );
    let graphql = read(&root, "docs/tutorial-graphql.md");
    assert!(
        graphql.contains("tutorial-react-todo.md"),
        "the GraphQL tutorial builds on the React tutorial and should link it"
    );
    assert!(
        graphql.contains("tutorial-ownership.md"),
        "the GraphQL tutorial leans on the ownership rules and should link them"
    );
}

/// The agents tutorial has to teach **the whole loop**, because every step of it
/// is a screen an admin has to be able to find — and a tutorial that quietly
/// lost one of them would still read fine. Each fragment below is one step:
/// connect a provider, give an agent a table, watch a tool call happen, hand it
/// a trigger, point it at code, and hang it off a trigger of its own.
#[test]
fn the_agents_tutorial_teaches_each_step_of_the_loop() {
    let root = workspace_root();
    let agents = read(&root, "docs/tutorial-agents.md");
    for fragment in [
        "Test connection",             // the provider, checked before it is saved
        "query_table",                 // the grant that lets an agent read a table
        "query_tasks",                 // …and the tool name its configuration derives
        "tool call",                   // what the transcript shows happening
        "Stop",                        // the abort, mid-answer
        "History",                     // …and where the run is afterwards
        "run_trigger",                 // the grant that lets an agent act
        "min_role",                    // …still gated by the trigger's own floor
        "`coding`",                    // the one trait the whole coding loop is
        "search_files",                // …grep,
        "edit_file",                   // …edit,
        "build_application",           // …build, and read the diagnostics
        "May create and change files", // the checkbox the edits are behind
        "no shell",                    // …and the script grant that ships instead of one
        "run_agent",                   // the agent as a trigger body (§11.5)
        "template literal",            // …whose prompt is a formula, written the safe way
        "max_steps",                   // the seatbelt
        "sentinel",                    // the redacted key
    ] {
        assert!(
            agents.contains(fragment),
            "the agents tutorial should cover `{fragment}`"
        );
    }
}

/// The React tutorial is where an admin learns the edit loop, and since the IDE
/// milestone that loop is **format → fix a type error → build**, in the
/// workbench rather than in the file manager's textarea. Each fragment below is
/// a step of it that would be invisibly lost if the section were rewritten
/// around the old file-manager loop: the tutorial would still read fine.
#[test]
fn the_react_tutorial_teaches_the_ide_loop() {
    let root = workspace_root();
    let react = read(&root, "docs/tutorial-react-todo.md");
    for fragment in [
        "/ide/?store=apps",     // how the workbench is reached, and from where
        "Format Document",      // …prettier, by the command's own name
        ".prettierrc",          // …with the project's own configuration
        "editor.formatOnSave",  // …and on save
        "TasksRow",             // the type a type error is caught against
        "Problems",             // where a diagnostic lands, from either source
        "node_modules",         // …which is why semantics need a build first
        "Saltcorn: Build Appl", // the build, from inside the editor
        "Source Control",       // and committing what was just edited
    ] {
        assert!(
            react.contains(fragment),
            "the React tutorial should cover `{fragment}`"
        );
    }
}

/// §12.1 is the IDE's design section, and the milestone deviated from it in
/// places that are load-bearing — a build that is served without a flag, a
/// language client that is not `monaco-languageclient`, an SCM view that is
/// deliberately a subset. A design document that still described the plan
/// instead of what was built would mislead the next person to read it, which is
/// the failure this test exists to catch.
#[test]
fn the_design_records_what_the_ide_milestone_actually_built() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    for fragment in [
        "IDE_CONTENT_SECURITY_POLICY",          // the relaxed policy, by name
        "SC_BUILD_ADMIN=0",                     // …and no `--ide-dir` to decide
        "before `initialize`",                  // the ordering the contributions depend on
        "monaco-languageclient` is not used",   // the language client deviation
        "does not match the server's root",     // …and the URI bridge it forced
        "close frame",                          // where a refusal is carried
        "Source control: the minimal SCM view", // the subset, and
        "Left out",                             // …what it leaves out
        "a diff against nothing",               // …for a stated reason
    ] {
        assert!(
            design.contains(fragment),
            "§12.1 should record `{fragment}`"
        );
    }
}

/// §11 is the agents milestone's design section, and the milestone deviated from
/// it in places that are load-bearing — a stop reason no vendor sends, a socket
/// protocol that settled differently, a trait signature that had to grow a
/// catalog, a seam that moved a layer down. The last two milestones proved this
/// is the step that is easy to skip and expensive to skip: a design document
/// that still described the plan would mislead the next person to read it.
///
/// Each phase records its own deviations, so this asserts that every phase's
/// block is still there and still names the thing that surprised it.
#[test]
fn the_design_records_what_the_agents_milestone_actually_built() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    for fragment in [
        // Every phase writes its deviations down under the same heading.
        "**What was built, where it deviates from the above** (Phase 1",
        "**What was built, where it deviates** (Phase 2)",
        "**What was built, where it deviates** (Phase 3, `query_table`)",
        "**What was built, where it deviates** (Phase 3, the write traits)",
        "**What was built, where it deviates** (Phase 3, `run_trigger`)",
        "**What was built, where it deviates** (Phase 4)",
        "**What was built, where it deviates** (Phase 5, the coding traits)",
        "**What was built, where it deviates from the above** (Phase 6)",
        // §11.1: the rig APIs that turned out to be wrong or missing.
        "`StopReason` has two variants", // no vendor sends one when streaming
        "merges consecutive tool results", // a wire-format obligation
        "defaults `max_tokens` to 4096", // …and a vendor requirement
        "`reqwest` moved to 0.13",       // one HTTP client in the build
        // §11.2: the extension point as it settled.
        "`AgentTrait::tools` takes the catalog",
        "`RunCaller` has two shapes and no default",
        // §11.4: the socket protocol as it settled.
        "The socket protocol, as it settled",
        "A tool call is emitted once",
        "answered with **silence**",
        // §11.5: the agent as a trigger body.
        "`ProviderConnector` moved down to `sc-agent`",
        "registered apart from the built-in action set",
        "triggered run is given no trigger dispatcher",
    ] {
        assert!(design.contains(fragment), "§11 should record `{fragment}`");
    }
}

/// The triggers tutorial teaches the three things Phases 2–8 built, and each of
/// them is a *screen* an admin has to be able to find: an `only_if` on a table
/// event, a `none` trigger reached through an application's API, and a periodic
/// one. A tutorial that quietly lost a third of that would still read fine,
/// which is exactly why it is worth a test.
#[test]
fn the_triggers_tutorial_covers_all_three_kinds_of_trigger() {
    let root = workspace_root();
    let triggers = read(&root, "docs/tutorial-triggers.md");
    for fragment in [
        "Only if",       // the per-row condition on a table event
        "old.done",      // …which is what makes it "became done" rather than "is done"
        "insert_row",    // the audit write
        "no event",      // the `none` kind, as the event picker spells it
        "/api/actions/", // reached through the app's API
        "Minimum role",  // guarded by the trigger's own floor
        "Once a day",    // the periodic kind, as the picker spells it
        "UTC",           // …which is the thing people get wrong
    ] {
        assert!(
            triggers.contains(fragment),
            "the triggers tutorial should cover `{fragment}`"
        );
    }
}

/// §13.4 is the GraphQL milestone's design section, and the parts worth having
/// written down are the ones somebody would otherwise have to read the provider
/// to learn: what the wire contract looks like, that the aggregate is *not*
/// implemented here but lowered onto `sc-expr`, the four authorization rules an
/// aggregate and a projected key made necessary, and the four bounds one
/// operation is served under. A section that lost any of them would still read
/// like a description of a GraphQL API, which is exactly why it is worth a test.
#[test]
fn the_design_records_what_the_graphql_milestone_actually_built() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    for fragment in [
        // The wire contract: two endpoints, one schema, the legacy HTTP rule.
        "POST {mount}",
        "schema.graphql",
        "200 with an `errors` array",
        "BigInt",                  // …and a scalar that does not silently lose information
        "FileValue { path, url }", // …nor become a second download path
        // Names are derived, and an underivable one is an omission.
        "<child>_by_<key>",
        "omitted with",
        // The load-bearing decision: the provider does not aggregate.
        "_sc_g1",
        "count(distinct: Column)",
        "row_number() OVER (PARTITION BY …)",
        // The four authorization rules.
        "ownership::aggregate_values_as",
        "ownership::join_guard",
        "never a quiet zero",
        "nullable", // …which is why a child list field is
        // What one operation may cost, and where each bound is counted.
        "max_complexity",
        "statement_budget",
        "statement is issued", // …two of which refuse before one ever is
        // …and the screen that drives it with the admin's own authority.
        "runApplicationGraphql",
    ] {
        assert!(
            design.contains(fragment),
            "§13.4 should record `{fragment}`"
        );
    }
}

/// The GraphQL tutorial has to reach the milestone's own query and then keep
/// going past the happy path: an admin is the one caller no rule applies to, so
/// a tutorial that stopped at "it works" would teach a GraphQL API with no
/// authorization and no cost. Each fragment below is one thing a reader would
/// otherwise have to discover in production.
#[test]
fn the_graphql_tutorial_reaches_the_motivating_query_and_its_rules() {
    let root = workspace_root();
    let tutorial = read(&root, "docs/tutorial-graphql.md");
    for fragment in [
        "employees_aggregate(where:", // the query the milestone exists for
        "_sc_g1",                     // …and the correlated subquery it becomes
        "row_number()",               // a nested `limit` is per parent
        "DataLoader",                 // …and a child list is one statement per level
        "insert_employees",           // the write path
        "BAD_USER_INPUT",             // …and what a refusal carries
        "x-csrf-token",               // calling it without a browser
        "errors in the body",         // …where a GraphQL endpoint puts its refusals
        "gql.tada",                   // typing it in the app, with no codegen step
        "partial results",            // what a caller who is not an admin sees
        "quiet zero",                 // …and the refusal that is never a count
        "32",                         // the statement budget, in the limits table
    ] {
        assert!(
            tutorial.contains(fragment),
            "the GraphQL tutorial should cover `{fragment}`"
        );
    }
}
