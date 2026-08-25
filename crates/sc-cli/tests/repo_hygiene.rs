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

/// Return the body of a TOML table header — everything from `[header]` up to the
/// next line that starts a new table. Used to assert a profile key is set *in
/// the right table*, which a whole-file `contains` cannot tell apart.
fn toml_section<'a>(toml: &'a str, header: &str) -> Option<&'a str> {
    let start = toml.find(header)? + header.len();
    let rest = &toml[start..];
    let end = rest
        .match_indices('\n')
        .find(|(i, _)| rest[i + 1..].starts_with('['))
        .map_or(rest.len(), |(i, _)| i);
    Some(&rest[..end])
}

/// The workspace links a static V8 into every one of its ~110 integration-test
/// binaries, so the debug-info budget in the workspace manifest is what keeps
/// `cargo test --workspace` from becoming a burst of ~440 MB links that drives
/// the session into `systemd-oomd`'s kill threshold — which, because oomd kills
/// a *cgroup*, takes the developer's whole terminal with it.
///
/// The budget is two keys, and dropping either puts the memory back without
/// breaking anything a test would notice, so each is asserted by name.
#[test]
fn the_workspace_keeps_debug_info_off_dependencies() {
    let root = workspace_root();
    let manifest = read(&root, "Cargo.toml");

    // Workspace crates keep line tables: a panicking test still prints a
    // backtrace with `file:line`, which is what a test run reads debug info for.
    let dev = toml_section(&manifest, "[profile.dev]")
        .unwrap_or_else(|| panic!("Cargo.toml must declare [profile.dev]"));
    assert!(
        dev.contains(r#"debug = "line-tables-only""#),
        "[profile.dev] should keep line tables only, so test backtraces still \
         carry file:line without paying for full DWARF; found: {dev:?}"
    );

    // Dependencies get none. This is the key that matters: it is the bulk of
    // both the binary size and the linker's peak memory. It needs no
    // counterpart under `[profile.test]` — `test` inherits from `dev`, and that
    // inheritance carries `package."*"` overrides too.
    let header = r#"[profile.dev.package."*"]"#;
    let deps = toml_section(&manifest, header)
        .unwrap_or_else(|| panic!("Cargo.toml must declare {header}"));
    assert!(
        deps.contains("debug = false"),
        "{header} must set `debug = false`: dependency DWARF is what took a test \
         binary to ~440 MB and a --workspace build to ~20 GB of peak memory; \
         found: {deps:?}"
    );
}

/// The budget above is only worth having if it is actually reaching the linker,
/// so this measures the output rather than the setting.
///
/// It deliberately does **not** measure *this* binary: `repo_hygiene` only reads
/// files, so the linker garbage-collects almost everything and it lands around
/// 7 MB whether the budget applies or not — it would pass either way and prove
/// nothing. The binaries that matter are the ones that really do pull in V8
/// (~173 MB with the budget, ~440 MB without), so this looks at the whole
/// `deps/` directory the current build wrote and checks the largest.
///
/// The ceiling is loose on purpose: it only has to separate ~173 from ~440, not
/// to police ordinary growth in the dependency tree.
///
/// A partial build (`-p sc-cli` alone) may have linked nothing large yet, in
/// which case there is simply nothing to measure and the test passes — this is a
/// second line of defence behind the manifest assertion above, which is the one
/// that always holds.
#[test]
fn linked_test_binaries_stay_within_the_debug_info_budget() {
    const CEILING_MB: u64 = 300;

    // `<target>/debug/deps/` — derived from this binary rather than assumed, so
    // it follows CARGO_TARGET_DIR and a `--target` build.
    let exe = std::env::current_exe().expect("a test binary knows its own path");
    let Some(deps) = exe.parent() else { return };

    let mut largest: Option<(PathBuf, u64)> = None;
    for entry in fs::read_dir(deps).into_iter().flatten().flatten() {
        let path = entry.path();
        // Test binaries have no extension; skip `.rlib`/`.rmeta`/`.d`/`.so`.
        if path.extension().is_some() {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if largest.as_ref().is_none_or(|(_, size)| meta.len() > *size) {
            largest = Some((path, meta.len()));
        }
    }

    let Some((path, size)) = largest else { return };
    let size_mb = size / (1024 * 1024);
    let name = path.file_name().unwrap_or_default().to_string_lossy();

    assert!(
        size_mb < CEILING_MB,
        "the largest linked test binary ({name}) is {size_mb} MB, over the \
         {CEILING_MB} MB ceiling. Either the debug-info budget in the workspace \
         Cargo.toml stopped applying, or this run deliberately overrode it \
         (`--config 'profile.dev.package.\"*\".debug=true'`), which is expected \
         to trip this test. Left unfixed, `cargo test --workspace` links ~110 \
         binaries this size at once and systemd-oomd kills the terminal it runs \
         in."
    );
}

/// The second layer, for a run that overruns anyway: `cargo-guarded.sh` puts
/// cargo in its own memory-capped cgroup, a *sibling* of the terminal's rather
/// than a child, so the cap can only take the build. The two properties worth
/// pinning are that it caps something and that it degrades to plain `cargo`
/// where there is no systemd — a wrapper that silently did nothing on one
/// machine and refused to run on another would be worse than no wrapper.
#[test]
fn the_guarded_cargo_wrapper_caps_memory_and_falls_back() {
    let root = workspace_root();
    let script = read(&root, "scripts/cargo-guarded.sh");

    assert!(
        script.contains("systemd-run") && script.contains("--scope"),
        "the wrapper must run cargo in its own transient scope"
    );
    assert!(
        script.contains("MemoryMax=") && script.contains("MemoryHigh="),
        "the wrapper must set both a throttle (MemoryHigh) and a wall (MemoryMax)"
    );
    assert!(
        script.contains(r#"exec cargo "$@""#),
        "the wrapper must fall back to plain cargo where systemd is unavailable"
    );

    // Executable, or `./scripts/cargo-guarded.sh` in the README does not work.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("scripts/cargo-guarded.sh");
        let mode = fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{path:?}: {e}"))
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "scripts/cargo-guarded.sh must be executable (mode is {mode:o})"
        );
    }
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
    assert!(
        triggers.contains("tutorial-workflows.md"),
        "the triggers tutorial should point at the workflows tutorial as a next step"
    );
    let workflows = read(&root, "docs/tutorial-workflows.md");
    assert!(
        workflows.contains("tutorial-triggers.md"),
        "the workflows tutorial builds on the triggers tutorial and should link it"
    );
    assert!(
        workflows.contains("tutorial-agents.md"),
        "the workflows tutorial should point at the agents tutorial as a next step"
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
    assert!(
        graphql.contains("tutorial-rest-queries.md"),
        "the GraphQL tutorial should point at the REST-queries tutorial as a next step"
    );
    let rest = read(&root, "docs/tutorial-rest-queries.md");
    assert!(
        rest.contains("tutorial-react-todo.md"),
        "the REST-queries tutorial builds on the React tutorial and should link it"
    );
    assert!(
        rest.contains("tutorial-constraints.md"),
        "the REST-queries tutorial should point at the constraints tutorial as a next step"
    );
    let constraints = read(&root, "docs/tutorial-constraints.md");
    assert!(
        constraints.contains("tutorial-ownership.md"),
        "the constraints tutorial is the other half of `the database decides` and should \
         link the ownership tutorial"
    );
    assert!(
        rest.contains("tutorial-ownership.md"),
        "the REST-queries tutorial leans on the ownership rules and should link them"
    );
    assert!(
        rest.contains("tutorial-graphql.md"),
        "the REST-queries tutorial should point at the questions GraphQL answers instead"
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
        "describe_action",             // …how it finds out what an action takes
        "save_trigger",                // …and writes the trigger that runs it
    ] {
        assert!(
            agents.contains(fragment),
            "the agents tutorial should cover `{fragment}`"
        );
    }
}

/// The workflows tutorial has to teach **the whole engine**, because each of
/// these is something a workflow author is stuck without and a document that
/// quietly lost one would still read fine: the five step kinds, the two ways a
/// run stops, the two ways it is answered, versioning, and — the one the engine
/// asks of *them* — what to do about a step that is not idempotent.
#[test]
fn the_workflows_tutorial_teaches_each_part_of_the_engine() {
    let root = workspace_root();
    let workflows = read(&root, "docs/tutorial-workflows.md");
    for fragment in [
        "A workflow",             // the trigger body that makes one
        "only_if",                // …and the condition that stops it starting itself
        "run_js_code",            // an Action step, and the one that does the reading
        "For each",               // the loop,
        "Item name",              // …and how its body names the item
        "User form",              // the wait for a person,
        "Give up after",          // …and the deadline that makes abandoning it a decision
        "Answers go to",          // …and where the answers land
        "Branch on a condition",  // control flow as data
        "Save a new version",     // versions are appended
        "Restore",                // …and a revert is a new one
        "pinned",                 // …which is what a suspended run finishes on
        "Restart the server",     // durability, demonstrated rather than claimed
        "at least once",          // the guarantee,
        "idempotent",             // …and the section about living with it
        "Retry, then fall through", // the error policies
        "context.error",          // …and what a handler reads
        "Cancel",                 // the two buttons a stuck run has
        "Retry from",             //
        "unknown identifier `context`", // the gap an author hits first (§10.3)
    ] {
        assert!(
            workflows.contains(fragment),
            "the workflows tutorial should cover `{fragment}`"
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
        // §11.3: delegation, added after the milestone closed.
        "**What was built, where it deviates** (`subagent`)",
        "Delegation, not handoff",
        "`TraitContext` carries a `Delegator`",
        "A cycle is refused by name, a chain by number",
        "Nothing came back means the delegation failed",
        // §11.5: the agent as a trigger body.
        "`ProviderConnector` moved down to `sc-agent`",
        "registered apart from the built-in action set",
        "triggered run is given no trigger dispatcher",
        // §11.3: the trigger half of `admin_copilot`, and the decision it turns
        // on — an action's settings are fetched when the model asks, not filled
        // in by a second, hidden inference call the way Saltcorn 1 did it.
        "**The triggers, and the problem they pose.**",
        "progressive disclosure inside the one loop",
        "A hidden second inference is a run nobody can read",
        "The four grants cover both halves",
        "`save_trigger` takes one trigger, not a list",
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

/// The API milestone spans three sections — query parameters in the endpoint
/// model (§13.1), the generated directory's contract (§13.3), and the REST query
/// string, custom SQL queries and per-provider configuration (§13.4) — and each
/// records something a reader would otherwise have to find in the source: the
/// data structure a filter vocabulary forced, the boundary between the generated
/// directory and the developer's project, the stated subset of PostgREST's
/// grammar, and the authority a raw statement does *and* does not carry.
#[test]
fn the_design_records_what_the_api_milestone_actually_built() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    for fragment in [
        // §13.1: a query parameter is part of the endpoint, and what that emits.
        "pub query:   Vec<QueryParam>",
        "A query parameter is part of the endpoint",
        "URLSearchParams",   // …which does the encoding, not concatenation
        "query_all",         // …and the two questions a handler asks
        "dropped predicate", // the failure a `HashMap` would hide
        // §13.3: the generated directory's contract, stated in the tree.
        "DatabaseDriver::render_ddl", // schema.sql is the driver's, not a second writer's
        "`AGENTS.md` goes at the project root",
        "emit_app_client",         // one re-emit, three callers
        "logged and never fatal",  // …inside somebody else's schema change
        "updateApplicationClient", // …and the same thing on demand
        // §13.4: configuration, the query string, and its stated subset.
        "ApiProviderInfo::config_spec",
        "supports_custom_queries",
        "syntax over the read layer",
        "one-to-many embeds",
        "ownership::join_guard",
        "reserved words a column",
        // §13.4: custom SQL, the one escape hatch, and its authority.
        "Statement::Raw { sql, binds }",
        "rewrite_named_params",
        "DatabaseDriver::describe",
        "READ ONLY",
        "impossible to save",
        "describeCustomQuery",
    ] {
        assert!(
            design.contains(fragment),
            "the design should record `{fragment}`"
        );
    }
}

/// The REST tutorial has to reach the milestone's own query and then keep going
/// past the happy path, for the same reason the GraphQL one does: an admin is the
/// caller no rule applies to, and a custom query is a hole somebody opens on
/// purpose. Each fragment below is one thing a reader would otherwise discover in
/// production.
#[test]
fn the_rest_tutorial_reaches_the_motivating_query_and_its_rules() {
    let root = workspace_root();
    let tutorial = read(&root, "docs/tutorial-rest-queries.md");
    for fragment in [
        // The query the milestone exists for, and every piece of its vocabulary.
        "select=title,published,author(name,country)",
        "is_null.true",
        "in.(200,300,400)",
        "order=published.desc,title",
        "Row cap per list read", // …and the ceiling an absent `limit` becomes
        // Nothing is ignored: the refusals, by the name of the thing refused.
        "one-to-many embeds",
        "!inner",
        "rows the caller did not ask for",
        // Reads that reach a second table are still reads.
        "read of the table it reaches",
        // The typed client, which is why a query parameter is in the endpoint.
        "ListBooksQuery",
        "Record<string, string>",
        "useQuery",
        // A custom query: written, checked, and what it is a hole in.
        "describeCustomQuery",
        "no table event",
        "READ ONLY",
        "saltcorn api add-query",
        "list-queries",
        "drop table books", // …an argument is a value, and the table survives
        "TopAuthorsResponse",
        // And the generated directory that keeps up with all of it.
        "schema.sql",
        "AGENTS.md",
        "Update code",
    ] {
        assert!(
            tutorial.contains(fragment),
            "the REST tutorial should cover `{fragment}`"
        );
    }
}

/// The workflow milestone, held to what it built (§10.3): the four things the
/// engine *is*, the guarantee that changed on contact with reality, and the two
/// gaps between the decisions and the code — which are the paragraphs a reader
/// is most harmed by losing, because each is a promise the plan made that the
/// code does not yet keep.
#[test]
fn the_design_records_what_the_workflow_milestone_actually_built() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    for fragment in [
        // A workflow is a trigger body, and its steps are versioned rows.
        "TriggerBody",
        "_sc_workflow_versions",
        "append-only",
        "subject_version",
        // Control flow is data, and the step set is five.
        "Control flow is data",
        "Five step kinds",
        "UserForm",
        // The machine, and what one advance guarantees.
        "no IO",
        "One advance is one atomic write",
        "at least once",
        // The queue, and why it is not the bus yet.
        "WorkQueue",
        "Recovery is not a special case",
        "started by `serve`",
        // The scope rule, and the gap in it.
        "workflow_shape",
        "Known gap",
        "unknown identifier `context`",
        // …and the second, smaller deviation.
        "granularity of such a loop is the loop, not the item",
        // The editor, and what was deliberately not built.
        "React Flow",
        "WorkflowRoom",
    ] {
        assert!(
            design.contains(fragment),
            "the design should record `{fragment}`"
        );
    }
}

/// The constraints milestone, held to what it built (§5.1): where a constraint
/// lives, what enforces a row constraint, and the two rules that are refusals
/// rather than features.
#[test]
fn the_design_records_what_the_constraints_milestone_actually_built() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    for fragment in [
        // Where a constraint lives, and why there is no table for it.
        "no `_sc_constraints`",
        "saltcorn_constraint",
        "AddUniqueConstraint",
        // What enforces a row constraint, and the shape of the generated body.
        "CONSTRAINT TRIGGER",
        "(SELECT (NEW).*)",
        "DEFERRABLE INITIALLY IMMEDIATE",
        "ERRCODE = 'check_violation'",
        // …and how the admin's own sentence gets back to the caller.
        "(constraint \"<name>\")",
        // The names, and the two refusals the schema editor owns.
        "sc_uq_<table>_<fields>",
        "dropped from under it",
        "rebuilt whenever its text fields change",
    ] {
        assert!(
            design.contains(fragment),
            "the design should record `{fragment}`"
        );
    }
}

/// The constraints tutorial has to reach all four kinds *and* the two things a
/// reader would otherwise meet in production: a rule refusing a write that never
/// went near Saltcorn, and a formula refused for asking a question the database
/// cannot answer.
#[test]
fn the_constraints_tutorial_covers_all_four_kinds_and_their_refusals() {
    let root = workspace_root();
    let tutorial = read(&root, "docs/tutorial-constraints.md");
    for fragment in [
        // The four kinds, by the name the screen calls them.
        "Jointly unique",
        "Full-text search",
        "Row constraint",
        "sc_uq_books_author_title",
        // The message, which is the whole reason the form asks for one.
        "You already have a book by that title.",
        // A formula that reaches another table — the case a CHECK cannot do.
        "authorⱵname",
        "booksↃauthor.length",
        // …and the two it may not ask.
        "the database has no session",
        "_insert",
        // Enforced where it counts.
        "INSERT INTO books",
        // Read back rather than stored, including somebody else's.
        "External",
        "no second copy",
        // The refusals the schema editor owns, and the deferral.
        "It is refused, by",
        "SET CONSTRAINTS ALL DEFERRED",
    ] {
        assert!(
            tutorial.contains(fragment),
            "the constraints tutorial should cover `{fragment}`"
        );
    }
}

/// Read one `[section]` of a `Cargo.toml`, returning the `sc-*` keys declared in
/// it. Deliberately a line scanner rather than a TOML parse: the manifests use
/// one dependency per line in both `sc-foo.workspace = true` and
/// `sc-foo = { … }` spellings, and this test has no business pulling a parser in
/// to read them.
fn manifest_section_sc_keys(manifest: &str, section: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == format!("[{section}]");
            continue;
        }
        if !inside || !trimmed.starts_with("sc-") {
            continue;
        }
        let key: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !key.is_empty() {
            keys.push(key);
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Every crate under `crates/`, with the `sc-*` crates it depends on directly
/// (dev-dependencies excluded — a dev-only edge is not part of the layering).
fn workspace_dependency_graph(root: &Path) -> Vec<(String, Vec<String>)> {
    let mut graph = Vec::new();
    let entries =
        fs::read_dir(root.join("crates")).unwrap_or_else(|e| panic!("missing crates/: {e}"));
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("unreadable {}: {e}", manifest_path.display()));
        let name = manifest
            .lines()
            .find_map(|l| l.trim().strip_prefix("name = \""))
            .and_then(|l| l.strip_suffix('"'))
            .unwrap_or_else(|| panic!("no package name in {}", manifest_path.display()))
            .to_owned();
        graph.push((name, manifest_section_sc_keys(&manifest, "dependencies")));
    }
    graph.sort();
    graph
}

/// The body of the `n`th fenced ```mermaid block in `doc`.
fn mermaid_block(doc: &str, n: usize) -> String {
    doc.split("```mermaid\n")
        .skip(1)
        .map(|rest| {
            rest.split_once("```")
                .unwrap_or_else(|| panic!("an unterminated ```mermaid block"))
                .0
                .to_owned()
        })
        .nth(n)
        .unwrap_or_else(|| panic!("the design document has no mermaid block {n}"))
}

/// §2's crate diagram and the dependency table under it are the picture of the
/// layering, and a picture that has drifted from `Cargo.toml` is worse than no
/// picture: it is read and believed. So the table is checked against the
/// manifests column for column, and every arrow in the diagram is checked to be
/// a dependency that actually exists.
///
/// The diagram is the graph's *transitive reduction*, so it is asserted to be a
/// subset of the real edges, not equal to them; the table carries the complete
/// lists, and that is what equality is asserted on.
#[test]
fn the_design_crate_diagram_matches_the_workspace_manifests() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    let actual = workspace_dependency_graph(&root);

    // The documented table: rows of "| `sc-x` | `sc-y` `sc-z` |".
    let mut documented: Vec<(String, Vec<String>)> = Vec::new();
    for line in design.lines() {
        let Some(rest) = line.strip_prefix("| `sc-") else {
            continue;
        };
        let Some((crate_cell, deps_cell)) = rest.split_once("` | ") else {
            continue;
        };
        let name = format!("sc-{crate_cell}");
        let mut deps: Vec<String> = deps_cell
            .trim_end_matches(" |")
            .split_whitespace()
            .filter_map(|tok| tok.strip_prefix('`')?.strip_suffix('`').map(str::to_owned))
            .filter(|tok| tok.starts_with("sc-"))
            .collect();
        deps.sort();
        documented.push((name, deps));
    }
    documented.sort();
    documented.dedup();

    // `sc-test-harness` lives under `tests/`, is a dev-dependency only, and is
    // deliberately absent from both the table and the diagram.
    let documented: Vec<_> = documented
        .into_iter()
        .filter(|(name, _)| name != "sc-test-harness")
        .collect();

    assert_eq!(
        documented, actual,
        "§2's direct-dependency table has drifted from the workspace manifests"
    );

    // Now the arrows. Nodes are declared as `id[\"sc-name\"]`; an edge is
    // `lhs --> rhs`, where either side may carry its declaration.
    let graph = mermaid_block(&design, 0);
    assert!(
        graph.trim_start().starts_with("graph "),
        "the first mermaid block in the design should be the crate graph"
    );
    let mut labels: Vec<(String, String)> = Vec::new();
    for token in graph.split_whitespace() {
        if let Some((id, rest)) = token.split_once("[\"")
            && let Some(name) = rest.strip_suffix("\"]")
        {
            labels.push((id.to_owned(), name.to_owned()));
        }
    }
    let resolve = |token: &str| -> String {
        let id = token.split_once("[\"").map_or(token, |(id, _)| id);
        labels
            .iter()
            .find(|(node, _)| node == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| panic!("the crate diagram uses undeclared node `{id}`"))
    };

    let mut drawn = 0usize;
    for line in graph.lines() {
        let parts: Vec<&str> = line.trim().split(" --> ").collect();
        if parts.len() != 2 {
            continue;
        }
        let (from, to) = (resolve(parts[0]), resolve(parts[1]));
        let deps = actual
            .iter()
            .find(|(name, _)| *name == from)
            .unwrap_or_else(|| panic!("the crate diagram draws `{from}`, which is not a crate"))
            .1
            .clone();
        assert!(
            deps.contains(&to),
            "the crate diagram draws `{from} --> {to}`, but {from} does not depend on {to}"
        );
        drawn += 1;
    }
    assert!(drawn > 20, "the crate diagram lost most of its arrows");

    // Every crate that exists is in the picture.
    for (name, _) in &actual {
        assert!(
            labels.iter().any(|(_, label)| label == name),
            "the crate diagram is missing `{name}`"
        );
    }
}

/// Every `_sc_*` table (and `users`) that some crate bootstraps must appear in
/// §9.2's entity-relationship diagram. A metadata table nobody drew is one an
/// admin discovers in `psql`, which is the failure §9 exists to prevent.
#[test]
fn the_er_diagram_names_every_metadata_table() {
    let root = workspace_root();
    let design = read(&root, "docs/TECHNICAL_DESIGN.md");
    let er = mermaid_block(&design, 1);
    assert!(
        er.trim_start().starts_with("erDiagram"),
        "the second mermaid block in the design should be the ER diagram"
    );

    let mut tables: Vec<String> = Vec::new();
    let mut sources = Vec::new();
    collect_rust_sources(&root.join("crates"), &mut sources);
    for path in &sources {
        let text = fs::read_to_string(path).unwrap_or_default();
        for line in text.lines() {
            // `const SOMETHING_TABLE: &str = "…";` — the `_TABLE` suffix is what
            // separates a table's name from the query-builder's `_sc_`-prefixed
            // column aliases, which are not tables and are not drawn.
            let trimmed = line.trim();
            let Some(rest) = trimmed
                .strip_prefix("pub const ")
                .or_else(|| trimmed.strip_prefix("const "))
                .or_else(|| {
                    trimmed
                        .split_once(") const ")
                        .filter(|(vis, _)| vis.starts_with("pub("))
                        .map(|(_, rest)| rest)
                })
            else {
                continue;
            };
            let Some((name, value)) = rest.split_once(": &str = \"") else {
                continue;
            };
            if !name.ends_with("_TABLE") {
                continue;
            }
            let Some((table, _)) = value.split_once('"') else {
                continue;
            };
            if table.starts_with("_sc_") || table == "users" {
                tables.push(table.to_owned());
            }
        }
    }
    tables.sort();
    tables.dedup();
    assert!(
        tables.len() >= 13,
        "expected the bootstrapped metadata tables, found {tables:?}"
    );
    for table in &tables {
        assert!(
            er.contains(&format!("\"{table}\"")),
            "§9.2's ER diagram does not draw `{table}`"
        );
    }
}

/// Every `.rs` file under `dir`, recursively.
fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The README's own section cross-references (`§7`, `§10`, …) must name a
/// section it actually has.
///
/// The document is written as numbered sections that point at each other, so
/// inserting one — §2's Debian quick start was inserted ahead of eight existing
/// sections — renumbers every reference after it. A stale `§5` is not a broken
/// link a reader can see through: it sends them to a section about something
/// else. References carrying a sub-section number (`§12.1`) or sitting next to
/// the words "design"/"TECHNICAL_DESIGN" are the *technical design's* sections,
/// not this document's, and are left alone.
#[test]
fn readme_section_references_resolve() {
    let root = workspace_root();
    let readme = read(&root, "README.md");

    let sections: Vec<u32> = readme
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .filter_map(|rest| rest.split_once(". "))
        .filter_map(|(n, _)| n.parse().ok())
        .collect();
    assert!(
        sections.len() >= 11,
        "expected the README's numbered sections, found {sections:?}"
    );

    for (idx, _) in readme.match_indices('§') {
        let rest = &readme[idx + '§'.len_utf8()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        // A sub-section number, or a reference the sentence attributes to the
        // design document: not ours to resolve.
        if rest[digits.len()..].starts_with('.') {
            continue;
        }
        let context = &readme[idx.saturating_sub(60)..idx];
        if context.contains("design") || context.contains("TECHNICAL_DESIGN") {
            continue;
        }
        let number: u32 = digits.parse().expect("digits");
        assert!(
            sections.contains(&number),
            "README references §{number}, which is not one of its sections {sections:?}"
        );
    }
}

/// The `saltcorn.toml` the Debian quick start (§2.5) tells an operator to write
/// must parse — with the *real* reader, the one the binary uses.
///
/// The file is `deny_unknown_fields` precisely so that a misspelled key fails
/// instead of quietly connecting somewhere else, which makes a sample in the
/// README a thing that can rot into a startup error on someone's first boot.
#[test]
fn readme_quick_start_config_file_parses() {
    let root = workspace_root();
    let readme = read(&root, "README.md");

    // The sample is the heredoc the quick start pipes into /etc/saltcorn.
    let (_, after) = readme
        .split_once("sudo tee /etc/saltcorn/saltcorn.toml >/dev/null <<'TOML'\n")
        .expect("§2.5 should write the configuration file with a TOML heredoc");
    let sample = after
        .split_once("\nTOML\n")
        .expect("the heredoc should be terminated")
        .0;

    let config = sc_cli::ConfigFile::parse(sample, Path::new("README.md#2.5"))
        .expect("the quick start's saltcorn.toml should parse");
    assert_eq!(config.default_environment.as_deref(), Some("production"));

    let production = config
        .environment("production", Path::new("README.md#2.5"))
        .expect("the quick start defines a production environment");
    // Peer authentication over the socket: a host that is a directory, a user,
    // a database — and deliberately no password to leave lying in /etc.
    assert_eq!(production.host.as_deref(), Some("/var/run/postgresql"));
    assert_eq!(production.user.as_deref(), Some("saltcorn"));
    assert_eq!(production.database.as_deref(), Some("saltcorn"));
    assert!(production.password.is_none() && production.url.is_none());
    // The serving half: without a base domain the server mounts no application.
    assert!(production.base_domain.is_some());
    assert!(production.bind.is_some());
}

/// The quick start's systemd unit has to carry the four lines that make the
/// service work as an unprivileged one, each of which is invisible until it is
/// missing: the state directory it may write to (`ProtectSystem=strict` makes
/// everything else read-only), a home for npm's cache when the server builds an
/// application, and the capability that lets a non-root process bind 80/443.
///
/// `Type=notify` and `WatchdogSec` are asserted too, because they are claims
/// about the *binary*: the server sends `READY=1` once the listener is bound and
/// pings the watchdog while it runs (`sc_server::ServiceManager`). A unit that
/// dropped either would silently give up a guarantee the code still provides;
/// one that kept them against a binary that stopped notifying would hang until
/// systemd's start timeout.
#[test]
fn readme_quick_start_systemd_unit_is_complete() {
    let root = workspace_root();
    let readme = read(&root, "README.md");
    let (_, after) = readme
        .split_once("sudo tee /etc/systemd/system/saltcorn.service >/dev/null <<'UNIT'\n")
        .expect("§2.6 should write the unit with a heredoc");
    let unit = after
        .split_once("\nUNIT\n")
        .expect("the heredoc should be terminated")
        .0;

    for line in [
        "Type=notify",
        "WatchdogSec=",
        "User=saltcorn",
        "StateDirectory=saltcorn",
        "ReadWritePaths=/var/lib/saltcorn",
        "Environment=HOME=/var/lib/saltcorn",
        "AmbientCapabilities=CAP_NET_BIND_SERVICE",
        "WantedBy=multi-user.target",
    ] {
        assert!(
            unit.contains(line),
            "the quick start's systemd unit should contain `{line}`"
        );
    }
    assert!(
        !unit.contains("Type=simple"),
        "the server notifies readiness, so the unit should claim it rather than Type=simple"
    );
}
