//! The workflow editor's arrival in `ui/admin` (design §10.3, phase 6), and the
//! two things about it that neither the Rust nor the TypeScript compiler can
//! check.
//!
//! 1. **It relaxed nothing.** Decision 9 admits React Flow *because* the admin
//!    CSP already carries `style-src 'unsafe-inline'` — Monaco needed it for the
//!    theme it writes into a `<style>` element at runtime, and React Flow's
//!    inline node transforms fit inside the same allowance. Nothing else may
//!    move: script sources stay `'self'` with no `eval` and no `blob:`, and no
//!    asset may come from another origin. A library that quietly wanted
//!    `unsafe-eval` or a CDN font would be a security change disguised as a
//!    dependency bump, so the policy is asserted **character for character**
//!    here, where a diff of this file is the review.
//! 2. **Its stylesheet is loaded the way every other one is** — imported through
//!    the bundler into the single same-origin `<link>`, not `@import`ed and not
//!    fetched from a CDN, which is the rule `admin_theme.rs` already keeps for
//!    Tabler and for the same reason: under `style-src 'self'` the browser
//!    refuses the second request and the canvas renders unstyled.
//!
//! The rest is the contract between the canvas component and the rules
//! `admin.css` keys on it, which fails silently: a renamed class gives back a
//! graph of unstyled boxes that still type-checks and still builds.
// `allow-expect-in-tests` covers `#[cfg(test)]` modules and not an integration
// test's own body, so every test file in this crate says so for itself.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use sc_server::CONTENT_SECURITY_POLICY;

/// The `ui/admin` directory.
fn ui() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/admin")
}

fn read(rel: &str) -> String {
    let path = ui().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_admin_csp_is_unchanged_by_the_editors_arrival() {
    // Stated in full rather than as a set of substring checks: the failure this
    // guards against is a directive *gaining* a source, and a `contains` test
    // passes right through that.
    assert_eq!(
        CONTENT_SECURITY_POLICY,
        "default-src 'self'; \
script-src 'self'; \
style-src 'self' 'unsafe-inline'; \
img-src 'self' data:; \
font-src 'self'; \
connect-src 'self'; \
base-uri 'none'; \
form-action 'self'; \
frame-ancestors 'none'; \
object-src 'none'",
        "the workflow editor must not relax the admin CSP: React Flow is admitted \
         by the `style-src 'unsafe-inline'` Monaco already needed, and nothing else"
    );

    // The relaxations the *IDE's* policy has are the IDE's; none of them may
    // arrive here on the back of a canvas library.
    for forbidden in ["unsafe-eval", "blob:", "worker-src", "frame-src"] {
        assert!(
            !CONTENT_SECURITY_POLICY.contains(forbidden),
            "the admin CSP must not carry `{forbidden}`"
        );
    }
}

#[test]
fn react_flow_is_a_dependency_loaded_through_the_bundler() {
    let package = read("package.json");
    // Decision 9's two libraries: the canvas, and the synchronous layout engine.
    assert!(
        package.contains("\"@xyflow/react\""),
        "ui/admin should depend on @xyflow/react (the workflow canvas, MIT)"
    );
    assert!(
        package.contains("\"@dagrejs/dagre\""),
        "ui/admin should depend on @dagrejs/dagre (the layout engine, MIT)"
    );

    // Same-origin, one `<link>`: the stylesheet is imported so the bundler emits
    // it into the single admin CSS file. A `@import` or a CDN `<link>` is a
    // second request `style-src 'self'` refuses, and the canvas would render
    // unstyled with the failure only visible in the console.
    let main = read("src/main.tsx");
    assert!(
        main.contains("@xyflow/react/dist/style.css"),
        "main.tsx should import React Flow's stylesheet through the bundler"
    );
    let index = read("index.html");
    for remote in ["http://", "https://", "//cdn", "unpkg", "jsdelivr"] {
        assert!(
            !index.contains(remote),
            "index.html must not reference `{remote}` — everything is same-origin"
        );
    }
}

#[test]
fn the_canvas_and_its_rules_agree() {
    let css = read("src/admin.css");
    let canvas = read("src/screens/WorkflowCanvas.tsx");

    for class in [
        // The container React Flow measures itself against. Without a height it
        // collapses and the graph is invisible.
        "workflow-canvas",
        // A step's card, the marker a computed `next` points at, and the handle a
        // loop's body hangs off.
        "wf-node",
        "wf-marker",
        "wf-handle-body",
        // The edge classes: the dashed ones, and the ones a run took.
        "wf-edge-dashed",
        "wf-edge-taken",
        "wf-edge-untaken",
    ] {
        assert!(
            canvas.contains(&format!("\"{class}")) || canvas.contains(&format!("{class}\"")),
            "WorkflowCanvas.tsx should render `.{class}`"
        );
        assert!(
            css.contains(&format!(".{class}")),
            "admin.css has no rules for `.{class}`, which WorkflowCanvas.tsx renders"
        );
    }

    // One colour per step kind, and per *kind* rather than per step: it is what
    // makes the shape of a workflow readable without opening five inspectors.
    // The class is built from the kind's own name, so a kind with no rule is a
    // node that silently loses its colour.
    for kind in ["action", "set", "for_each", "wait", "user_form"] {
        assert!(
            css.contains(&format!(".wf-node-{kind}")),
            "admin.css has no colour for a `{kind}` step"
        );
    }
    // …and the three marks a run leaves on the same nodes.
    for mark in ["visited", "current", "failed"] {
        assert!(
            css.contains(&format!(".wf-node-{mark}")),
            "admin.css has no rule for a `{mark}` step on a run's canvas"
        );
    }
}

#[test]
fn the_editor_screens_are_routed_and_state_their_titles() {
    let app = read("src/App.tsx");
    // A workflow is a trigger **body** (decision 1), so its editor and its runs
    // hang off the trigger's id — there is deliberately no `/workflows/…` route,
    // because there is no workflow to address without a trigger.
    for route in [
        "/workflow",
        "/runs",
        "WorkflowEditor",
        "WorkflowRuns",
        "RunDetail",
    ] {
        assert!(
            app.contains(route),
            "App.tsx should route the workflow screens (missing `{route}`)"
        );
    }
    assert!(
        !app.contains("\"/workflows"),
        "a workflow is a trigger body, not an entity with a route of its own"
    );

    for screen in ["WorkflowEditor", "WorkflowRuns", "RunDetail"] {
        let source = read(&format!("src/screens/{screen}.tsx"));
        assert!(
            source.contains("<PageHeader"),
            "{screen}.tsx should render its title with <PageHeader>"
        );
    }
}

#[test]
fn the_editor_renders_a_steps_settings_from_the_actions_own_declaration() {
    // The promise §13.3 makes about frameworks and file-store backends, kept for
    // workflow steps: a plugin's action gets a working step form with no change
    // to the inspector, because the form is `SettingsFields` over whatever
    // `listActions` declared. A hand-written form for some particular action is
    // exactly what this is here to catch.
    let inspector = read("src/screens/WorkflowInspector.tsx");
    assert!(
        inspector.contains("<SettingsFields") && inspector.contains("config_spec"),
        "the step inspector should render an action's settings from its config_spec"
    );

    // …and the same component renders the form a *person* answers, which is why
    // resuming a suspended run needs no code that knows what a workflow is.
    let detail = read("src/screens/RunDetail.tsx");
    assert!(
        detail.contains("<SettingsFields") && detail.contains("pending_form"),
        "the run detail should render a pending form from the step's own declaration"
    );

    // The palette offers only actions the server said can be a step here
    // (phase 5.4) — decided from the declaration, not from a list of names.
    let editor = read("src/screens/WorkflowEditor.tsx");
    assert!(
        editor.contains("workflow_step"),
        "the step palette should offer only actions `listActions` marked usable as a step"
    );
}
