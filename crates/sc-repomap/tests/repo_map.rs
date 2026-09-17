//! The map end to end (TODO 7.4): real files parsed, ranked for a focus, and
//! fitted to a budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use sc_repomap::{Focus, SourceFile, TagCache, estimate_tokens, repo_map};

/// A small React project: an app, a list, an API module, a settings screen
/// nothing else uses, a Python script and a README.
fn project(cache: &TagCache) -> Vec<SourceFile> {
    let files: &[(&str, &str)] = &[
        (
            "src/App.tsx",
            "import { TaskList } from './TaskList';\n\
             export function App() {\n  return <TaskList filter=\"all\" />;\n}\n",
        ),
        (
            "src/TaskList.tsx",
            "import { listTasks, Filter } from './api';\n\
             export interface TaskListProps {\n  filter: Filter;\n}\n\
             export function TaskList({ filter }: TaskListProps) {\n  const tasks = listTasks(filter);\n  return <ul>{tasks.length}</ul>;\n}\n",
        ),
        (
            "src/api.ts",
            "export type Filter = 'all' | 'done';\n\
             export function listTasks(filter: Filter) {\n  return fetchJson('/api/tasks?f=' + filter);\n}\n\
             export function fetchJson(url: string) {\n  return fetch(url);\n}\n",
        ),
        (
            "src/Settings.tsx",
            "export function SettingsScreen() {\n  return <form>{readSettingsFromStorage()}</form>;\n}\n\
             export function readSettingsFromStorage() {\n  return localStorage.getItem('s');\n}\n",
        ),
        ("scripts/seed.py", "def seed_database():\n    pass\n"),
        ("README.md", "# Tasks\n"),
    ];
    files
        .iter()
        .map(|(path, source)| SourceFile {
            path: (*path).to_owned(),
            tags: cache.tags(path, source.as_bytes()),
        })
        .collect()
}

#[test]
fn the_whole_map_names_every_file_and_its_signatures() {
    let cache = TagCache::new();
    let map = repo_map(&project(&cache), &Focus::none(), 10_000);
    for line in [
        "src/api.ts",
        "    2│ export function listTasks(filter: Filter) {",
        "    1│ export type Filter = 'all' | 'done';",
        "src/TaskList.tsx",
        "src/Settings.tsx",
        "scripts/seed.py",
        "    1│ def seed_database():",
        "README.md",
    ] {
        assert!(map.lines().any(|l| l == line), "`{line}` in:\n{map}");
    }
}

#[test]
fn a_fitted_map_is_under_budget_and_starts_with_the_focus() {
    let cache = TagCache::new();
    let files = project(&cache);
    let known: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let focus = Focus::from_terms(["src/Settings.tsx"], &known);

    for budget in [15, 40, 80, 200] {
        let map = repo_map(&files, &focus, budget);
        assert!(estimate_tokens(&map) <= budget, "{budget}: {map}");
        if !map.is_empty() {
            assert!(map.starts_with("src/Settings.tsx\n"), "{budget}:\n{map}");
        }
    }

    // Focused on the list instead, what it uses comes straight after it.
    let focus = Focus::from_terms(["src/TaskList.tsx"], &known);
    let map = repo_map(&files, &focus, 200);
    let files_in_order: Vec<&str> = map.lines().filter(|l| !l.starts_with(' ')).collect();
    assert_eq!(
        files_in_order[..2],
        ["src/TaskList.tsx", "src/api.ts"],
        "{map}"
    );
    // A name in the focus pulls its definition up.
    let focus = Focus::from_terms(["fetchJson"], &known);
    let map = repo_map(&files, &focus, 30);
    assert!(map.contains("fetchJson"), "{map}");
}
