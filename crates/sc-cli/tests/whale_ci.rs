//! Guards `whale-ci.yml` and the Dockerfiles it builds — the pipeline that runs
//! this workspace's tests in containers.
//!
//! None of this runs docker: what can go wrong with a CI definition is not that
//! a container misbehaves but that the definition drifts away from the tree it
//! tests, and that is checkable here, in a second, rather than in a forty-minute
//! run that fails at its last step. Three kinds of drift are caught:
//!
//! 1. **Dangling references.** A step's `dockerfile:` must exist, an `image:`
//!    naming another step and every `depends:` must name a declared step, and a
//!    hostname a step connects to must be a *service* it depends on — whale-ci
//!    resolves step names as hostnames, so a typo is a connection refused
//!    halfway through a build.
//! 2. **Opt-in gates renamed.** The suite's rule is that a test needing the
//!    network is `#[ignore]`d or behind an environment variable; the pipeline is
//!    where those are turned on. If `SC_TEST_NPM` or `SC_TEST_MQTT_BROKER` is
//!    renamed in the source, the YAML keeps setting a variable nothing reads and
//!    the tests it was meant to run go on silently skipping.
//! 3. **Named tests renamed.** Two steps run single tests by name, for the same
//!    reason: a renamed test is a filter that matches nothing, and `cargo test`
//!    calls running zero tests a success.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Walk up from this crate's manifest dir to the workspace root.
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
        assert!(
            dir.pop(),
            "reached the filesystem root without a [workspace] Cargo.toml"
        );
    }
}

/// The pipeline, as a map from step name to the raw text of its block.
///
/// A hand-rolled split rather than a YAML parser: the workspace carries no YAML
/// dependency, and every question below is about a top-level key and the lines
/// under it, which indentation answers exactly.
fn steps(yaml: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in yaml.lines() {
        let is_top_level_key = !line.starts_with([' ', '\t', '#', '-'])
            && line.trim_end().ends_with(':')
            && !line.trim().is_empty();
        if is_top_level_key {
            let name = line.trim_end().trim_end_matches(':').to_string();
            current = Some(name.clone());
            out.insert(name, String::new());
        } else if let Some(name) = &current {
            out.get_mut(name)
                .expect("the current step is in the map")
                .push_str(line);
            out.get_mut(name)
                .expect("the current step is in the map")
                .push('\n');
        }
    }
    out
}

/// Every value of `key` in a step's block, whether written inline (`depends: a`)
/// or as a list underneath it.
fn values(block: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list = false;
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let rest = rest.trim();
            if rest.is_empty() {
                in_list = true;
            } else {
                out.push(rest.to_string());
                in_list = false;
            }
        } else if in_list {
            match trimmed.strip_prefix("- ") {
                Some(item) => out.push(item.trim().to_string()),
                None => in_list = false,
            }
        }
    }
    out
}

fn pipeline() -> (PathBuf, String, BTreeMap<String, String>) {
    let root = workspace_root();
    let yaml = fs::read_to_string(root.join("whale-ci.yml")).expect("whale-ci.yml is missing");
    let steps = steps(&yaml);
    (root, yaml, steps)
}

#[test]
fn every_dockerfile_the_pipeline_builds_exists() {
    let (root, _, steps) = pipeline();
    let mut built = 0;
    for (name, block) in &steps {
        for path in values(block, "dockerfile") {
            let path = root.join(path.trim_start_matches("./"));
            assert!(
                path.is_file(),
                "step `{name}` builds {}, which does not exist",
                path.display()
            );
            built += 1;
        }
    }
    assert!(
        built >= 2,
        "the pipeline should build both its images, found {built}"
    );
}

#[test]
fn every_step_reference_names_a_declared_step() {
    let (_, _, steps) = pipeline();
    for (name, block) in &steps {
        for dep in values(block, "depends") {
            assert!(
                steps.contains_key(&dep),
                "step `{name}` depends on `{dep}`, which is not a step"
            );
        }
        // An `image:` that is a step name reuses that step's built image; one
        // that is not is pulled from a registry, and `postgres:16` and the like
        // are exactly that. Only the bare, registry-shaped-free names are
        // checked, which is the class a typo lands in.
        for image in values(block, "image") {
            if image == "build" {
                assert!(
                    steps.contains_key(&image),
                    "step `{name}` runs in the `{image}` image, which is not a step"
                );
            }
        }
    }
}

#[test]
fn a_step_that_connects_to_a_service_depends_on_it() {
    let (_, _, steps) = pipeline();
    for (name, block) in &steps {
        // The two hostnames a step reaches: the database it is pointed at, and
        // the broker. Both are written as step names, which is how whale-ci
        // resolves them.
        let mut hosts = Vec::new();
        for url in values(block, "DATABASE_URL") {
            let host = url
                .rsplit_once('@')
                .expect("a DATABASE_URL with credentials")
                .1;
            hosts.push(host.split([':', '/']).next().unwrap().to_string());
        }
        for url in values(block, "SC_TEST_MQTT_BROKER") {
            let host = url.trim_start_matches("mqtt://");
            hosts.push(host.split([':', '/']).next().unwrap().to_string());
        }
        for host in hosts {
            let service = steps.get(&host).unwrap_or_else(|| {
                panic!("step `{name}` connects to `{host}`, which is not a step")
            });
            assert!(
                values(service, "service").first().map(String::as_str) == Some("true"),
                "step `{name}` connects to `{host}`, which is not a service"
            );
            assert!(
                values(block, "depends").contains(&host),
                "step `{name}` connects to `{host}` without depending on it, so it may start first"
            );
        }
    }
}

#[test]
fn the_opt_in_gates_it_sets_are_the_ones_the_tests_read() {
    let (root, yaml, _) = pipeline();
    // Each variable, and a file that reads it. The pipeline setting a variable
    // no test reads is a step that quietly runs nothing.
    for (var, reader) in [
        ("SC_TEST_NPM", "crates/sc-app/tests/scaffold_app.rs"),
        ("SC_TEST_MQTT_BROKER", "crates/sc-module/tests/host.rs"),
        ("SC_TSC", "crates/sc-api/tests/typescript_typecheck.rs"),
        ("DATABASE_URL", "tests/harness/src/lib.rs"),
    ] {
        assert!(
            yaml.contains(var) || var == "SC_TSC",
            "the pipeline no longer sets {var}"
        );
        let source = fs::read_to_string(root.join(reader))
            .unwrap_or_else(|e| panic!("missing {reader}: {e}"));
        assert!(
            source.contains(var),
            "{reader} no longer reads {var}, but CI still sets it"
        );
    }
    // `SC_TSC` is set in the image rather than in the YAML, since every Rust
    // step wants it.
    let dockerfile = fs::read_to_string(root.join("deploy/whale-ci/Dockerfile.build"))
        .expect("Dockerfile.build");
    assert!(
        dockerfile.contains("SC_TSC"),
        "the Rust image no longer points sc-api's type-check test at a tsc"
    );
}

#[test]
fn the_tests_it_names_still_exist() {
    let (root, yaml, _) = pipeline();
    for (test, file) in [
        (
            "a_scaffolded_app_installs_builds_and_serves_end_to_end",
            "crates/sc-app/tests/scaffold_app.rs",
        ),
        (
            "the_bundled_vue_module_scaffolds_an_application_that_builds",
            "crates/sc-module/tests/bundled_vue.rs",
        ),
    ] {
        assert!(yaml.contains(test), "the pipeline no longer runs {test}");
        let source =
            fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("missing {file}: {e}"));
        assert!(
            source.contains(&format!("fn {test}")),
            "{file} no longer defines {test}, so CI's filter matches nothing — and a run of zero \
             tests is a pass"
        );
    }
}

#[test]
fn the_rust_image_carries_what_this_workspace_cannot_build_without() {
    let (root, _, _) = pipeline();
    let dockerfile = fs::read_to_string(root.join("deploy/whale-ci/Dockerfile.build"))
        .expect("Dockerfile.build");
    for required in [
        // bindgen, through `deno_runtime` → `rusqlite`'s session feature.
        "libclang-dev",
        // `aws-lc-sys` and rusqlite's bundled amalgamation.
        "cmake",
        // `libz-sys`, via `deno_node`.
        "zlib1g-dev",
        // `sc-python`'s embedded interpreter, and the venv its tests build.
        "python3-dev",
        // The role the harness connects as is created with psql.
        "postgresql-client",
        // rustfmt and clippy are not in a `--profile minimal` toolchain.
        "rustup component add rustfmt clippy",
    ] {
        assert!(
            dockerfile.contains(required),
            "Dockerfile.build no longer provides `{required}`"
        );
    }
}
