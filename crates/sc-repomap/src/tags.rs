//! Tags: what a file defines and what it refers to (TODO 7.2).
//!
//! A [`Tag`] is a name, whether this is its definition or a reference to it,
//! its line, and for a definition the line's text: the signature the map shows.
//! Tree-sitter finds them with the vendored `queries/*.scm` (`tree-sitter-tags`
//! does the matching). A file in a language with no grammar has no tags, and
//! is still in the map by name.
//!
//! Parsing is the expensive part of building a map and files rarely change
//! between two maps of the same store, so tags are kept in a [`TagCache`] keyed
//! by the language and the content's hash: a file is parsed again only when its
//! bytes change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

/// The most characters of a definition's line kept as its signature.
pub const MAX_SIGNATURE_CHARS: usize = 120;

/// Files larger than this are not parsed: generated bundles and data files say
/// nothing a map should.
pub const MAX_PARSED_BYTES: usize = 512 * 1024;

/// Whether a tag defines its name or refers to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Definition,
    Reference,
}

/// One name in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub kind: TagKind,
    /// 1-based.
    pub line: u32,
    /// What it is, as the query named it: `function`, `class`, `call`, …
    pub syntax: String,
    /// The definition's line, trimmed and clipped. Empty for a reference.
    pub signature: String,
}

/// A language the map can parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    JavaScript,
    TypeScript,
    Tsx,
    Python,
}

impl Language {
    /// The language of a path, by its extension. `None` for a file the map
    /// lists by name only, and for everything when built without `grammars`.
    pub fn for_path(path: &str) -> Option<Language> {
        if !cfg!(feature = "grammars") {
            return None;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        let (_, extension) = name.rsplit_once('.')?;
        match extension {
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "py" => Some(Language::Python),
            _ => None,
        }
    }

    /// The vendored tags query.
    pub fn query(self) -> &'static str {
        match self {
            Language::JavaScript => include_str!("../queries/javascript.scm"),
            Language::TypeScript => include_str!("../queries/typescript.scm"),
            Language::Tsx => include_str!("../queries/tsx.scm"),
            Language::Python => include_str!("../queries/python.scm"),
        }
    }
}

/// The tags of `source`, a file in `language`. A file that does not parse, or
/// is too large to, has none.
pub fn extract(language: Language, source: &[u8]) -> Vec<Tag> {
    if source.len() > MAX_PARSED_BYTES {
        return Vec::new();
    }
    parse(language, source)
}

#[cfg(feature = "grammars")]
fn parse(language: Language, source: &[u8]) -> Vec<Tag> {
    use tree_sitter_tags::TagsContext;

    thread_local! {
        static CONTEXT: std::cell::RefCell<TagsContext> = std::cell::RefCell::new(TagsContext::new());
    }
    let Some(config) = configuration(language) else {
        return Vec::new();
    };
    CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let Ok((tags, _)) = context.generate_tags(config, source, None) else {
            return Vec::new();
        };
        tags.filter_map(Result::ok)
            .filter_map(|tag| {
                let name = std::str::from_utf8(source.get(tag.name_range.clone())?).ok()?;
                let line = u32::try_from(tag.span.start.row + 1).ok()?;
                let signature = match tag.is_definition {
                    true => signature(source.get(tag.line_range.clone())?),
                    false => String::new(),
                };
                Some(Tag {
                    name: name.to_owned(),
                    kind: match tag.is_definition {
                        true => TagKind::Definition,
                        false => TagKind::Reference,
                    },
                    line,
                    syntax: config.syntax_type_name(tag.syntax_type_id).to_owned(),
                    signature,
                })
            })
            .collect()
    })
}

#[cfg(not(feature = "grammars"))]
fn parse(_language: Language, _source: &[u8]) -> Vec<Tag> {
    Vec::new()
}

/// The compiled query for `language`, built once per process.
#[cfg(feature = "grammars")]
fn configuration(language: Language) -> Option<&'static tree_sitter_tags::TagsConfiguration> {
    use std::sync::OnceLock;
    use tree_sitter_tags::TagsConfiguration;

    static JAVASCRIPT: OnceLock<Option<TagsConfiguration>> = OnceLock::new();
    static TYPESCRIPT: OnceLock<Option<TagsConfiguration>> = OnceLock::new();
    static TSX: OnceLock<Option<TagsConfiguration>> = OnceLock::new();
    static PYTHON: OnceLock<Option<TagsConfiguration>> = OnceLock::new();
    let (cell, grammar): (_, tree_sitter::Language) = match language {
        Language::JavaScript => (&JAVASCRIPT, tree_sitter_javascript::LANGUAGE.into()),
        Language::TypeScript => (
            &TYPESCRIPT,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        Language::Tsx => (&TSX, tree_sitter_typescript::LANGUAGE_TSX.into()),
        Language::Python => (&PYTHON, tree_sitter_python::LANGUAGE.into()),
    };
    cell.get_or_init(|| TagsConfiguration::new(grammar, language.query(), "").ok())
        .as_ref()
}

/// Whether `language`'s query compiles against its grammar. For tests.
pub fn query_compiles(language: Language) -> Result<(), String> {
    #[cfg(feature = "grammars")]
    {
        use tree_sitter_tags::TagsConfiguration;
        let grammar: tree_sitter::Language = match language {
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
        };
        TagsConfiguration::new(grammar, language.query(), "")
            .map(|_| ())
            .map_err(|e| format!("{language:?}: {e}"))
    }
    #[cfg(not(feature = "grammars"))]
    {
        let _ = language;
        Err("built without the `grammars` feature".to_owned())
    }
}

/// A definition's line as the map shows it.
#[cfg(feature = "grammars")]
fn signature(line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    if text.chars().count() <= MAX_SIGNATURE_CHARS {
        return text.to_owned();
    }
    let cut: String = text.chars().take(MAX_SIGNATURE_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// The cache's map: language and content hash to the tags.
type Entries = HashMap<(Language, [u8; 32]), Arc<Vec<Tag>>>;

/// Tags by language and content hash.
///
/// Bounded: past [`TagCache::CAPACITY`] entries it is emptied and refilled,
/// which costs one round of parsing and keeps a long-lived server from holding
/// every version of every file it ever mapped.
#[derive(Default)]
pub struct TagCache {
    entries: Mutex<Entries>,
}

impl TagCache {
    /// The most files' tags kept.
    pub const CAPACITY: usize = 20_000;

    /// An empty cache.
    pub fn new() -> TagCache {
        TagCache::default()
    }

    /// The tags of `source` at `path`: from the cache when these bytes were
    /// parsed before, otherwise parsed now and kept. A path in no known
    /// language has none, and is not cached.
    pub fn tags(&self, path: &str, source: &[u8]) -> Arc<Vec<Tag>> {
        let Some(language) = Language::for_path(path) else {
            return Arc::new(Vec::new());
        };
        let key = (language, Sha256::digest(source).into());
        if let Some(hit) = self.lock().get(&key) {
            return hit.clone();
        }
        let tags = Arc::new(extract(language, source));
        let mut entries = self.lock();
        if entries.len() >= Self::CAPACITY {
            entries.clear();
        }
        entries.insert(key, tags.clone());
        tags
    }

    /// How many files' tags are kept.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(all(test, feature = "grammars"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn defs(tags: &[Tag]) -> Vec<(&str, u32, &str)> {
        tags.iter()
            .filter(|t| t.kind == TagKind::Definition)
            .map(|t| (t.name.as_str(), t.line, t.syntax.as_str()))
            .collect()
    }

    fn refs(tags: &[Tag]) -> Vec<&str> {
        let mut names: Vec<&str> = tags
            .iter()
            .filter(|t| t.kind == TagKind::Reference)
            .map(|t| t.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    #[test]
    fn every_vendored_query_compiles_against_its_grammar() {
        for language in [
            Language::JavaScript,
            Language::TypeScript,
            Language::Tsx,
            Language::Python,
        ] {
            query_compiles(language).unwrap();
        }
    }

    #[test]
    fn a_path_names_its_language_by_extension() {
        assert_eq!(Language::for_path("src/App.tsx"), Some(Language::Tsx));
        assert_eq!(Language::for_path("src/api.ts"), Some(Language::TypeScript));
        assert_eq!(Language::for_path("index.mjs"), Some(Language::JavaScript));
        assert_eq!(Language::for_path("tool/run.py"), Some(Language::Python));
        assert_eq!(Language::for_path("README.md"), None);
        assert_eq!(Language::for_path("Makefile"), None);
    }

    #[test]
    fn a_tsx_component_defines_and_uses_what_the_map_needs() {
        let source = b"import { listTasks } from './api';\n\
            \n\
            export type Filter = 'all' | 'done';\n\
            \n\
            export interface Props {\n  filter: Filter;\n}\n\
            \n\
            export function TaskList({ filter }: Props) {\n  const tasks = listTasks(filter);\n  return <ul>{tasks.map((t) => <TaskItem task={t} />)}</ul>;\n}\n\
            \n\
            export const App = () => <TaskList filter=\"all\" />;\n";
        let tags = extract(Language::Tsx, source);
        assert_eq!(
            defs(&tags),
            vec![
                ("Filter", 3, "type"),
                ("Props", 5, "interface"),
                ("TaskList", 9, "function"),
                ("App", 14, "function"),
            ]
        );
        let task_list = tags.iter().find(|t| t.name == "TaskList").unwrap();
        assert_eq!(
            task_list.signature,
            "export function TaskList({ filter }: Props) {"
        );
        let used = refs(&tags);
        for name in ["Filter", "Props", "TaskItem", "TaskList", "listTasks"] {
            assert!(used.contains(&name), "{name} in {used:?}");
        }
    }

    #[test]
    fn javascript_and_python_have_tags_too() {
        let js = extract(
            Language::JavaScript,
            b"class Store {\n  load() { return fetchAll(); }\n}\nfunction fetchAll() {}\n",
        );
        assert_eq!(
            defs(&js),
            vec![
                ("Store", 1, "class"),
                ("load", 2, "method"),
                ("fetchAll", 4, "function")
            ]
        );
        assert_eq!(refs(&js), vec!["fetchAll"]);

        let py = extract(
            Language::Python,
            b"from app.db import connect\n\nclass Repo:\n    def rows(self):\n        return connect().rows()\n",
        );
        assert_eq!(
            defs(&py),
            vec![("Repo", 3, "class"), ("rows", 4, "function")]
        );
        assert_eq!(refs(&py), vec!["connect", "rows"]);
    }

    #[test]
    fn unchanged_bytes_are_not_parsed_again() {
        let cache = TagCache::new();
        let first = cache.tags("a.ts", b"export function one() {}\n");
        let again = cache.tags("b/renamed.ts", b"export function one() {}\n");
        assert!(Arc::ptr_eq(&first, &again), "same bytes, same entry");
        assert_eq!(cache.len(), 1);
        let changed = cache.tags("a.ts", b"export function two() {}\n");
        assert_eq!(changed[0].name, "two");
        assert_eq!(cache.len(), 2);
        // A file in no known language is not cached and has no tags.
        assert!(cache.tags("notes.md", b"# one").is_empty());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn a_long_signature_is_clipped_and_a_huge_file_is_skipped() {
        let long = format!("export function f({}) {{}}\n", "a, ".repeat(80));
        let tags = extract(Language::TypeScript, long.as_bytes());
        assert!(tags[0].signature.ends_with('…'));
        assert!(tags[0].signature.chars().count() <= MAX_SIGNATURE_CHARS + 1);
        let huge = "export function f() {}\n".repeat(MAX_PARSED_BYTES / 20);
        assert!(extract(Language::TypeScript, huge.as_bytes()).is_empty());
    }
}
