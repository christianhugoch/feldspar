//! `plugins/react-native` — the bundled React Native (Expo) module — installed
//! from the directory it ships in, and asked for a project.
//!
//! The same questions `bundled_vue.rs` asks of the Vue module: does the
//! declaration translate, does every relative import in the generated project
//! resolve, and does a rebuild leave the developer's screens alone. More are this
//! framework's own: it declares an Android build target, its screens use no DOM
//! (the same source is the web bundle and the APK), and a phone's requests go to
//! the application's URL rather than to a page it does not have.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{have_npm, temp_root};
use sc_app::{DeclaredFile, FilePhase, FrameworkHost};
use sc_module::{
    BundledModules, Installer, LoadedModule, Module, ModuleFrameworks, ModuleHost,
    ModulePermissions, ModuleSet, ModuleSource,
};
use sc_types::Attrs;
use serde_json::{Value as Json, json};

/// The framework's name, as an application row stores it.
const FRAMEWORK: &str = "react-native";

/// The four files Saltcorn writes into the runtime directory for every
/// framework, which the module's project may import and must not write.
const SALTCORN_WRITES: [&str; 4] = ["client.ts", "helper.ts", "schema.sql", "SKILL.md"];

/// The bundled module, installed into a throwaway root and loaded on a worker.
async fn frameworks(tag: &str) -> ModuleFrameworks {
    frameworks_configured(tag, json!({})).await.0
}

/// [`frameworks`] with the module's own settings — what the Modules tab saves —
/// answering the manifest too, for what the module declared about them.
async fn frameworks_configured(
    tag: &str,
    configuration: Json,
) -> (ModuleFrameworks, sc_module::ModuleManifest) {
    let catalog = BundledModules::discover(None);
    let entry = catalog
        .get("react-native")
        .expect("the React Native module ships");
    let root = temp_root(tag);
    let installer = Installer::new(&root);
    let package = installer
        .install(ModuleSource::Local, &entry.directory.display().to_string())
        .await
        .unwrap_or_else(|e| panic!("installing plugins/react-native: {e}"));
    assert_eq!(package.name, entry.name);

    let host = Arc::new(ModuleHost::new(&root));
    let manifest = host
        .load(
            &package.name,
            &installer.package_dir(&package.name),
            &configuration,
            &ModulePermissions::closed(),
        )
        .await
        .expect("the bundled React Native module loads");
    assert!(
        manifest.issues.is_empty(),
        "the module this server ships should declare nothing it has to apologise for: {:?}",
        manifest.issues
    );

    let module = Module::new(
        &package.name,
        ModuleSource::Bundled,
        entry.directory.display().to_string(),
    );
    let set = ModuleSet::empty().merged(vec![LoadedModule {
        module,
        manifest: Some(manifest.clone()),
        config_spec: Vec::new(),
        issues: Vec::new(),
    }]);
    let frameworks = ModuleFrameworks::new(&host, &set);
    assert!(
        frameworks.issues().is_empty(),
        "the declaration translates cleanly: {:?}",
        frameworks.issues()
    );
    (frameworks, manifest)
}

/// A writable table with a numeric key and all three control kinds.
fn tasks_table() -> Json {
    json!({
        "name": "tasks",
        "pascal": "Tasks",
        "title": "Tasks",
        "client": "tasks",
        "pk": "id",
        "pk_ts_type": "number",
        "storable": true,
        "ops": { "list": true, "get": true, "create": true, "update": true, "delete": true },
        "fields": [
            { "name": "id", "label": "Id", "control": "number", "ts_type": "number",
              "empty": "0", "required": true, "primary_key": true },
            { "name": "title", "label": "Title", "control": "text", "ts_type": "string",
              "empty": "\"\"", "required": true, "primary_key": false },
            { "name": "estimate", "label": "Estimate", "control": "number", "ts_type": "number",
              "empty": "0", "required": false, "primary_key": false },
            { "name": "done", "label": "Done", "control": "checkbox", "ts_type": "boolean",
              "empty": "false", "required": false, "primary_key": false }
        ],
        "form": {
            "inputs": [
                { "name": "title", "label": "Title", "control": "text", "ts_type": "string",
                  "empty": "\"\"", "required": true, "primary_key": false },
                { "name": "estimate", "label": "Estimate", "control": "number", "ts_type": "number",
                  "empty": "0", "required": false, "primary_key": false },
                { "name": "done", "label": "Done", "control": "checkbox", "ts_type": "boolean",
                  "empty": "false", "required": false, "primary_key": false }
            ],
            "minted": []
        }
    })
}

/// A read-only table with no addressable row.
fn readings_table() -> Json {
    json!({
        "name": "readings",
        "pascal": "Readings",
        "title": "Readings",
        "client": "readings",
        "pk": null,
        "pk_ts_type": "string",
        "storable": false,
        "ops": { "list": true, "get": false, "create": false, "update": false, "delete": false },
        "fields": [
            { "name": "at", "label": "At", "control": "text", "ts_type": "string",
              "empty": "\"\"", "required": true, "primary_key": false }
        ],
        "form": { "inputs": [], "minted": [] }
    })
}

fn context(auth: bool, tables: Vec<Json>) -> Json {
    json!({
        "project": "todo",
        "name": "todo",
        "runtime": "src/feldspar",
        "client": "src/feldspar/client.ts",
        "auth": auth,
        "app": { "name": "My Todo", "subdomain": "todo", "description": "",
                 "url": "https://todo.example.com" },
        "tables": tables,
        "graphql": null,
        "schema_sql": "CREATE TABLE tasks ();",
        "roles": [{ "name": "Admin", "role": 1, "description": "" }],
        "skill": "# SKILL\n"
    })
}

async fn scaffold(tag: &str, auth: bool, tables: Vec<Json>) -> Vec<DeclaredFile> {
    frameworks(tag)
        .await
        .framework_files(FRAMEWORK, FilePhase::Scaffold, context(auth, tables))
        .await
        .expect("the scaffold runs")
}

fn file(files: &[DeclaredFile], path: &str) -> String {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path}"))
        .contents
        .clone()
}

/// Every relative import in every generated file, resolved against what the
/// project will actually contain.
fn assert_imports_resolve(files: &[DeclaredFile]) {
    let mut present: BTreeSet<String> = files.iter().map(|f| f.path.clone()).collect();
    for name in SALTCORN_WRITES {
        present.insert(format!("src/feldspar/{name}"));
    }
    for file in files {
        let dir = file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        for spec in relative_imports(&file.contents) {
            let resolved = resolve(dir, &spec);
            let found = ["", ".ts", ".tsx"]
                .iter()
                .any(|ext| present.contains(&format!("{resolved}{ext}")));
            assert!(
                found,
                "{} imports `{spec}`, which resolves to `{resolved}` and nothing writes it. \
                 The project has: {present:?}",
                file.path
            );
        }
    }
}

/// The `./x` and `../x` specifiers of every `import`/`export … from` in a file.
fn relative_imports(contents: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (index, _) in contents.match_indices("from \"") {
        let rest = &contents[index + 6..];
        let Some(end) = rest.find('"') else { continue };
        let spec = &rest[..end];
        if spec.starts_with("./") || spec.starts_with("../") {
            out.push(spec.to_owned());
        }
    }
    out
}

/// `src/screens` + `../feldspar/hooks` → `src/feldspar/hooks`.
fn resolve(dir: &str, spec: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for segment in spec.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[tokio::test]
async fn the_bundled_react_native_module_declares_a_web_framework() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let frameworks = frameworks("bundled-rn-decl").await;
    let declared = frameworks.frameworks();
    assert_eq!(declared.len(), 1);
    let rn = &declared[0];
    assert_eq!(rn.name, FRAMEWORK);
    assert_eq!(rn.info().label, "React Native");
    assert!(rn.scaffolds);
    assert_eq!(rn.checks, ["typecheck"]);
    assert_eq!(
        rn.config_spec.iter().map(|f| f.name()).collect::<Vec<_>>(),
        ["store", "project", "mobile_url"]
    );

    let config: Attrs = [
        ("store".to_owned(), json!("apps")),
        ("project".to_owned(), json!("todo")),
    ]
    .into_iter()
    .collect();
    let spec = rn.build_spec(&config).unwrap();
    assert_eq!(spec.command, "npm");
    assert_eq!(spec.args, ["run", "build"]);
    assert_eq!(spec.output_dir, "todo/dist");
    assert_eq!(
        rn.client_path(&config).unwrap().unwrap(),
        "todo/src/feldspar/client.ts"
    );

    // The APK is a build target beside the web bundle, run in the same project.
    assert_eq!(rn.targets.len(), 1);
    let apk = rn.target_spec("android", &config).unwrap();
    assert_eq!(apk.label, "Android APK");
    assert_eq!(apk.command, "npm");
    assert_eq!(apk.args, ["run", "build:android"]);
    assert_eq!(apk.source_dir, "todo");
    assert_eq!(
        apk.artifact,
        "todo/android/app/build/outputs/apk/release/app-release.apk"
    );

    // One widening beyond React's policy: react-native-web injects its styles at
    // run time. Scripts stay strict.
    let csp = rn.default_csp();
    assert_eq!(csp.directives["style-src"], ["'self'", "'unsafe-inline'"]);
    assert!(!csp.directives.contains_key("script-src"));
    assert!(!csp.header_value().contains("unsafe-eval"));
    assert_eq!(csp.directives["default-src"], ["'self'"]);
    assert_eq!(csp.directives["frame-ancestors"], ["'none'"]);
}

/// The Android toolchain is the module's own setting, edited on the Modules
/// tab, and it reaches the APK build as `ANDROID_HOME` and `JAVA_HOME` — so the
/// server does not depend on the shell that started it.
#[tokio::test]
async fn the_modules_toolchain_settings_are_the_apk_builds_environment() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (configured, manifest) = frameworks_configured(
        "bundled-rn-toolchain",
        json!({ "android_home": "/opt/android-sdk", "java_home": "" }),
    )
    .await;
    // The two settings the Modules tab shows for this module.
    let names: Vec<&str> = manifest
        .config_fields
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert_eq!(names, ["android_home", "java_home"]);

    let config: Attrs = [("store".to_owned(), json!("apps"))].into_iter().collect();
    let apk = configured.frameworks()[0]
        .target_spec("android", &config)
        .unwrap();
    assert_eq!(
        apk.env.get("ANDROID_HOME").map(String::as_str),
        Some("/opt/android-sdk")
    );
    // A setting left blank leaves the variable to the server's environment.
    assert!(!apk.env.contains_key("JAVA_HOME"), "{:?}", apk.env);

    // And the target says what it needs, so a server without the toolchain is
    // told before Gradle runs: both directories, met by these settings or by the
    // server's own environment.
    let names: Vec<String> = apk
        .requires
        .iter()
        .map(|r| match &r.kind {
            sc_app::TargetRequirementKind::Env { name, directory } => {
                assert!(directory, "{name} must be a directory");
                name.clone()
            }
            other => panic!("an Android build needs only its directories: {other:?}"),
        })
        .collect();
    assert_eq!(names, ["ANDROID_HOME", "JAVA_HOME"]);
    assert!(
        apk.requires
            .iter()
            .all(|r| r.hint.contains("Settings → Modules"))
    );

    // And unconfigured, the build carries no environment of its own at all.
    let apk = frameworks("bundled-rn-toolchain-none").await.frameworks()[0]
        .target_spec("android", &config)
        .unwrap();
    assert!(apk.env.is_empty(), "{:?}", apk.env);
}

#[tokio::test]
async fn the_scaffold_is_an_expo_project_whose_every_import_resolves() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let files = scaffold(
        "bundled-rn-scaffold",
        true,
        vec![tasks_table(), readings_table()],
    )
    .await;

    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    for expected in [
        "package.json",
        "app.json",
        "tsconfig.json",
        "AGENTS.md",
        "index.ts",
        "src/Root.tsx",
        "src/App.tsx",
        "src/routes.tsx",
        "src/navigation.tsx",
        "src/theme.ts",
        "src/auth.tsx",
        "src/Login.tsx",
        "src/screens/Home.tsx",
        "src/screens/Tasks.tsx",
        "src/screens/Readings.tsx",
        "src/feldspar/hooks.ts",
        "src/feldspar/config.ts",
        "src/feldspar/README.md",
    ] {
        assert!(paths.contains(&expected), "missing {expected}: {paths:?}");
    }
    for name in SALTCORN_WRITES {
        let path = format!("src/feldspar/{name}");
        assert!(!paths.contains(&path.as_str()), "the module wrote {path}");
    }
    assert_imports_resolve(&files);

    // One Expo project: the web bundle is `expo export`, the APK is prebuild plus
    // Gradle, both over the same dependencies.
    let package = file(&files, "package.json");
    for dependency in ["\"expo\":", "\"react-native\":", "\"react-native-web\":"] {
        assert!(package.contains(dependency), "{dependency}: {package}");
    }
    assert!(
        package.contains("tsc --noEmit && expo export --platform web --output-dir dist"),
        "{package}"
    );
    assert!(
        package.contains("\"typecheck\": \"tsc --noEmit\""),
        "{package}"
    );
    assert!(
        package.contains("\"build:android\": \"expo prebuild --platform android"),
        "{package}"
    );
    assert!(file(&files, "index.ts").contains("registerRootComponent(Root)"));
    let app_json = file(&files, "app.json");
    assert!(
        app_json.contains("\"package\": \"com.feldspar.todo\""),
        "{app_json}"
    );
    assert!(app_json.contains("\"output\": \"single\""), "{app_json}");
    // Served over HTTPS, so the APK asks for no cleartext permission.
    assert!(!app_json.contains("usesCleartextTraffic"), "{app_json}");
    assert!(!package.contains("expo-build-properties"), "{package}");

    // A phone has no page to be relative to: its requests go to the app's URL.
    let hooks = file(&files, "src/feldspar/hooks.ts");
    assert!(
        hooks.contains("baseUrl: Platform.OS === \"web\" ? \"\" : APP_URL"),
        "{hooks}"
    );
    assert!(
        file(&files, "src/feldspar/config.ts")
            .contains("export const APP_URL = \"https://todo.example.com\";")
    );

    // The screens are React Native, not DOM: that is what a native target needs.
    for path in paths.iter().filter(|p| p.starts_with("src/screens/")) {
        let screen = file(&files, path);
        assert!(screen.contains("from \"react-native\""), "{path}");
        for dom in ["<div", "<input", "<button", "className"] {
            assert!(!screen.contains(dom), "{path} uses `{dom}`:\n{screen}");
        }
    }

    // The hooks are typed per table; a keyless read-only table gets the read only.
    assert!(hooks.starts_with("// Code generated by Saltcorn. DO NOT EDIT."));
    assert!(hooks.contains("export function useTasks(): Query<TasksRow[]>"));
    assert!(hooks.contains("export function useDeleteTasks(): Mutation<number>"));
    assert!(hooks.contains("export function useReadings()"));
    assert!(!hooks.contains("useDeleteReadings") && !hooks.contains("useCreateReadings"));

    // Each control kind is the React Native component for it.
    let tasks = file(&files, "src/screens/Tasks.tsx");
    assert!(tasks.contains("useCreateTasks") && tasks.contains("useDeleteTasks"));
    assert!(tasks.contains("<Switch value={form.done}"), "{tasks}");
    assert!(tasks.contains("value={form.title}"), "{tasks}");
    assert!(tasks.contains("value={String(form.estimate)}"), "{tasks}");
    // …and a screen with no form imports none of what a form needs.
    let readings = file(&files, "src/screens/Readings.tsx");
    for unused in [
        "useState",
        "TextInput",
        "Switch",
        "Pressable",
        "useCreateReadings",
    ] {
        assert!(!readings.contains(unused), "{unused} in:\n{readings}");
    }

    // The shell gates on the signed-in user.
    let app = file(&files, "src/App.tsx");
    assert!(
        app.contains("<Login />") && app.contains("signOut"),
        "{app}"
    );
    assert!(file(&files, "src/routes.tsx").contains("path: \"/tasks\""));
}

#[tokio::test]
async fn an_app_that_cannot_sign_anyone_in_gets_no_auth_layer() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let files = scaffold("bundled-rn-anon", false, vec![tasks_table()]).await;
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(!paths.contains(&"src/auth.tsx"));
    assert!(!paths.contains(&"src/Login.tsx"));
    assert_imports_resolve(&files);
    for path in ["src/App.tsx", "src/Root.tsx"] {
        let contents = file(&files, path);
        assert!(
            !contents.contains("Login") && !contents.contains("Auth"),
            "{path}:\n{contents}"
        );
    }
}

#[tokio::test]
async fn an_app_with_no_tables_still_gets_a_project_that_imports_cleanly() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let files = scaffold("bundled-rn-empty", true, vec![]).await;
    assert_imports_resolve(&files);
    let home = file(&files, "src/screens/Home.tsx");
    assert!(
        !home.contains("useNavigation"),
        "a home screen with nothing to link to must not import the navigator: {home}"
    );
}

#[tokio::test]
async fn an_app_served_over_http_lets_its_apk_reach_it() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let mut ctx = context(true, vec![tasks_table()]);
    ctx["app"]["url"] = json!("http://todo.192.168.1.50.nip.io:3032");
    let files = frameworks("bundled-rn-cleartext")
        .await
        .framework_files(FRAMEWORK, FilePhase::Scaffold, ctx)
        .await
        .expect("the scaffold runs");
    // A release APK refuses plain HTTP unless it says otherwise, and a
    // development server is plain HTTP.
    let app_json = file(&files, "app.json");
    assert!(
        app_json.contains("\"usesCleartextTraffic\": true"),
        "{app_json}"
    );
    assert!(file(&files, "package.json").contains("\"expo-build-properties\":"));
    assert!(
        file(&files, "src/feldspar/config.ts").contains("http://todo.192.168.1.50.nip.io:3032")
    );
}

#[tokio::test]
async fn the_mobile_url_setting_is_where_the_apk_sends_its_requests() {
    skip_without!(have_npm(), "npm is not on the PATH");
    // An emulator cannot reach the application's own URL, so the admin names one
    // it can: the setting wins over the URL, and a trailing slash is dropped.
    let mut ctx = context(true, vec![tasks_table()]);
    ctx["settings"] = json!({ "mobile_url": " http://todo.10.0.2.2.nip.io:3032/ " });
    let files = frameworks("bundled-rn-mobile-url")
        .await
        .framework_files(FRAMEWORK, FilePhase::Scaffold, ctx)
        .await
        .expect("the scaffold runs");
    assert!(
        file(&files, "src/feldspar/config.ts")
            .contains("export const APP_URL = \"http://todo.10.0.2.2.nip.io:3032\";")
    );
    // That URL is plain HTTP, so the APK may speak it — although the
    // application itself is served over HTTPS.
    assert!(file(&files, "app.json").contains("\"usesCleartextTraffic\": true"));
}

#[tokio::test]
async fn a_rebuild_rewrites_the_generated_directory_and_nothing_else() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let files = frameworks("bundled-rn-runtime")
        .await
        .framework_files(
            FRAMEWORK,
            FilePhase::Runtime,
            context(true, vec![tasks_table()]),
        )
        .await
        .expect("the runtime runs");
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "src/feldspar/hooks.ts",
            "src/feldspar/config.ts",
            "src/feldspar/README.md"
        ],
        "a build must not rewrite a screen the developer owns"
    );
}

/// Install → scaffold → `npm install` → `tsc` → `expo export`, with a real
/// database, a real file store and a real Node toolchain.
///
/// Opt-in (`SC_TEST_NPM=1`), like the Vue module's: it needs npm and a network.
/// It is the only test that type-checks the generated screens and hooks against
/// the generated client and React Native's own types. The APK target is not
/// built here: that needs an Android SDK and is minutes long.
#[tokio::test]
async fn the_bundled_react_native_module_scaffolds_an_application_that_builds() {
    if std::env::var("SC_TEST_NPM").as_deref() != Ok("1") {
        eprintln!("skipping: set SC_TEST_NPM=1 to run the real npm build");
        return;
    }
    skip_without!(have_npm(), "npm is not on the PATH");

    let frameworks = frameworks("bundled-rn-e2e").await;
    sc_app::install_frameworks(sc_app::FrameworkSet::new(Arc::new(frameworks)))
        .expect("the registry is not poisoned");

    let db = sc_test_harness::TestDb::new()
        .await
        .expect("a test database");
    db.client()
        .await
        .expect("a client")
        .batch_execute(
            "CREATE TABLE tasks (\
               id bigint generated by default as identity primary key, \
               title text not null, \
               estimate integer, \
               done boolean)",
        )
        .await
        .expect("the app's table");
    let driver = Arc::new(sc_db_postgres::PgDriver::from_pool(db.pool().clone()));
    let cat = sc_catalog::Catalog::init(driver as Arc<dyn sc_db::DatabaseDriver>)
        .await
        .expect("a catalog");

    let dir = temp_root("bundled-rn-project");
    std::fs::create_dir_all(&dir).expect("the store's directory");
    cat.connect_file_store(Arc::new(
        sc_files::LocalFileStore::new("apps", &dir).expect("a store"),
    ))
    .expect("the store connects");
    sc_app::bootstrap(&cat)
        .await
        .expect("the applications table");

    let app = sc_app::Application::new(
        "Todo",
        "todo",
        sc_app::FrameworkRef::new(FRAMEWORK)
            .with("store", "apps")
            .with("project", "todo"),
    )
    .with_table(sc_catalog::TableId("tasks".to_owned()))
    .with_file_store(sc_catalog::FileStoreId("apps".to_owned()))
    .with_api(sc_app::ApiConfig::new("rest", "/api"));
    let app = sc_app::save_application(&cat, &app)
        .await
        .expect("a `react-native` application saves like any other");

    let report = sc_app::scaffold_app(&cat, &app, None)
        .await
        .expect("the module's scaffold runs");
    assert!(
        report
            .files
            .iter()
            .any(|f| f == "todo/src/screens/Tasks.tsx")
    );

    let source = sc_app::app_source_from_config(&app.framework).expect("a source");
    let built = sc_app::build_application(&cat, &app, &source, None)
        .await
        .expect("the scaffolded project type-checks and builds");
    assert!(built.installed, "the first build installs dependencies");

    let fw = sc_app::CodeFramework::new(FRAMEWORK, built.bundle);
    let index = fw.serve(&sc_app::AppRequest::get("/"));
    assert_eq!(index.status, 200);
    assert!(String::from_utf8_lossy(&index.body).contains("<div id=\"root\">"));
    assert_eq!(fw.serve(&sc_app::AppRequest::get("/tasks")).status, 200);

    std::fs::remove_dir_all(&dir).ok();
}
