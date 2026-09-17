//! The graph and the ranking (TODO 7.3): Aider's algorithm.
//!
//! **The graph.** Every file is a node. For each name some file defines and some
//! file refers to, the referring file gets an edge to each defining file,
//! weighted by how much the name is likely to matter:
//!
//! - √(times it is referred to from that file), so a name used everywhere does
//!   not drown the rest;
//! - ×10 when the name is in the focus, ×10 when it is a long identifier with
//!   structure (`useTaskList`, `load_rows`), ×0.1 when it is private (`_x`),
//!   ×0.1 when more than five files define it (a `render` or an `index`);
//! - ×50 when the referring file is in the focus.
//!
//! A definition nobody refers to is not an edge: it gets a hundredth of its
//! own file's rank, so it still ranks, below what that file's rank buys the
//! names other files use. (Aider gives it a weak self-edge instead, which hands
//! it the *whole* rank of a file nothing else links out of.) Where a tree has
//! no references at all, the definitions stand in for them.
//!
//! **The ranking.** PageRank over the weighted graph, **personalised**: the
//! random walk restarts at the focus files rather than anywhere, so what they
//! use ranks above what the rest of the tree uses. Each file's rank is then
//! shared out along its edges to the definitions they name. Hand-rolled power
//! iteration: a graph of a few thousand files converges in a few dozen passes.
//!
//! **The order.** Definitions in focus files first, then every other definition,
//! each by rank; then the files with no ranked definition, by name. Ties break
//! on path and line, so a tree ranks the same way twice.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::SourceFile;
use crate::tags::{Tag, TagKind};

/// The PageRank damping factor.
const DAMPING: f64 = 0.85;
/// The most power-iteration passes.
const MAX_ITERATIONS: usize = 100;
/// When a pass changes the ranks by less than this in total, they have
/// converged.
const TOLERANCE: f64 = 1e-10;
/// The share of its file's rank a definition nobody refers to gets.
const UNREFERENCED_SHARE: f64 = 0.01;

/// What the agent is working on: files (paths as [`SourceFile::path`] spells
/// them) and names (identifiers mentioned in the request or the feature).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Focus {
    pub files: BTreeSet<String>,
    pub names: BTreeSet<String>,
}

impl Focus {
    /// A focus on nothing: the map of the whole tree, ranked by use.
    pub fn none() -> Focus {
        Focus::default()
    }

    /// Split `terms` into files (those `known` has) and names (the rest that
    /// look like identifiers).
    pub fn from_terms<'a>(
        terms: impl IntoIterator<Item = &'a str>,
        known: &BTreeSet<&str>,
    ) -> Focus {
        let mut focus = Focus::default();
        for term in terms {
            let term = term.trim().trim_start_matches("./").trim_matches('`');
            if term.is_empty() {
                continue;
            }
            if known.contains(term) {
                focus.files.insert(term.to_owned());
            } else if term
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                && term.chars().next().is_some_and(|c| !c.is_ascii_digit())
            {
                focus.names.insert(term.to_owned());
            }
        }
        focus
    }
}

/// One entry of the ranked map.
#[derive(Debug, Clone, PartialEq)]
pub enum Ranked {
    /// A definition, with its share of rank.
    Definition { path: String, tag: Tag, rank: f64 },
    /// A file with nothing ranked in it, listed by name.
    File { path: String },
}

impl Ranked {
    /// The file the entry is in.
    pub fn path(&self) -> &str {
        match self {
            Ranked::Definition { path, .. } | Ranked::File { path } => path,
        }
    }
}

/// Rank every definition in `files` for `focus`.
pub fn rank(files: &[SourceFile], focus: &Focus) -> Vec<Ranked> {
    let index: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();

    // Who defines each name, and how often each file refers to it.
    let mut defines: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    let mut references: BTreeMap<&str, BTreeMap<usize, usize>> = BTreeMap::new();
    for (i, file) in files.iter().enumerate() {
        for tag in file.tags.iter() {
            match tag.kind {
                TagKind::Definition => {
                    defines.entry(&tag.name).or_default().insert(i);
                }
                TagKind::Reference => {
                    *references
                        .entry(&tag.name)
                        .or_default()
                        .entry(i)
                        .or_default() += 1;
                }
            }
        }
    }
    if references.is_empty() {
        for (name, definers) in &defines {
            references.insert(name, definers.iter().map(|d| (*d, 1)).collect());
        }
    }

    // Edges, per (from, to, name), and the per-node sums the walk uses.
    let focus_files: BTreeSet<usize> = focus
        .files
        .iter()
        .filter_map(|p| index.get(p.as_str()).copied())
        .collect();
    let mut edges: Vec<(usize, usize, &str, f64)> = Vec::new();
    let mut unreferenced: Vec<(usize, &str)> = Vec::new();
    for (name, definers) in &defines {
        let Some(referrers) = references.get(name) else {
            unreferenced.extend(definers.iter().map(|d| (*d, *name)));
            continue;
        };
        let mut weight = 1.0;
        if focus.names.contains(*name) {
            weight *= 10.0;
        }
        if is_structured(name) && name.chars().count() >= 8 {
            weight *= 10.0;
        }
        if name.starts_with('_') {
            weight *= 0.1;
        }
        if definers.len() > 5 {
            weight *= 0.1;
        }
        for (referrer, count) in referrers {
            let from_focus = if focus_files.contains(referrer) {
                50.0
            } else {
                1.0
            };
            for definer in definers {
                edges.push((
                    *referrer,
                    *definer,
                    name,
                    weight * from_focus * (*count as f64).sqrt(),
                ));
            }
        }
    }

    let ranks = pagerank(files.len(), &edges, &focus_files);

    // Each file's rank, shared along its edges to the definitions they name.
    let mut out_weight = vec![0.0; files.len()];
    for (from, _, _, weight) in &edges {
        out_weight[*from] += weight;
    }
    let mut shares: BTreeMap<(usize, &str), f64> = BTreeMap::new();
    for (from, to, name, weight) in &edges {
        if out_weight[*from] > 0.0 {
            *shares.entry((*to, name)).or_default() += ranks[*from] * weight / out_weight[*from];
        }
    }
    for (file, name) in unreferenced {
        *shares.entry((file, name)).or_default() += ranks[file] * UNREFERENCED_SHARE;
    }

    let mut definitions: Vec<(bool, f64, usize, &Tag)> = Vec::new();
    for ((file, name), share) in &shares {
        for tag in files[*file]
            .tags
            .iter()
            .filter(|t| t.kind == TagKind::Definition && t.name == *name)
        {
            definitions.push((focus_files.contains(file), *share, *file, tag));
        }
    }
    definitions.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then_with(|| files[a.2].path.cmp(&files[b.2].path))
            .then(a.3.line.cmp(&b.3.line))
    });

    let ranked_files: BTreeSet<usize> = definitions.iter().map(|d| d.2).collect();
    let mut rest: Vec<usize> = (0..files.len())
        .filter(|i| !ranked_files.contains(i))
        .collect();
    rest.sort_by(|a, b| {
        focus_files
            .contains(b)
            .cmp(&focus_files.contains(a))
            .then_with(|| files[*a].path.cmp(&files[*b].path))
    });

    definitions
        .into_iter()
        .map(|(_, rank, file, tag)| Ranked::Definition {
            path: files[file].path.clone(),
            tag: tag.clone(),
            rank,
        })
        .chain(rest.into_iter().map(|file| Ranked::File {
            path: files[file].path.clone(),
        }))
        .collect()
}

/// Whether a name has structure: snake_case, kebab-case or camelCase.
fn is_structured(name: &str) -> bool {
    let has_letter = name.chars().any(char::is_alphabetic);
    let snake = name.contains('_') && has_letter;
    let kebab = name.contains('-') && has_letter;
    let camel = name.chars().any(char::is_uppercase) && name.chars().any(char::is_lowercase);
    snake || kebab || camel
}

/// PageRank over `n` nodes and weighted `edges`, restarting at `personal` (or
/// anywhere, when it is empty). Returns ranks summing to one.
pub fn pagerank(
    n: usize,
    edges: &[(usize, usize, &str, f64)],
    personal: &BTreeSet<usize>,
) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let restart: Vec<f64> = match personal.is_empty() {
        true => vec![1.0 / n as f64; n],
        false => (0..n)
            .map(|i| match personal.contains(&i) {
                true => 1.0 / personal.len() as f64,
                false => 0.0,
            })
            .collect(),
    };
    let mut out_weight = vec![0.0; n];
    for (from, _, _, weight) in edges {
        out_weight[*from] += weight;
    }
    let mut ranks = restart.clone();
    for _ in 0..MAX_ITERATIONS {
        let dangling: f64 = (0..n)
            .filter(|i| out_weight[*i] <= 0.0)
            .map(|i| ranks[i])
            .sum();
        let mut next: Vec<f64> = restart
            .iter()
            .map(|p| (1.0 - DAMPING) * p + DAMPING * dangling * p)
            .collect();
        for (from, to, _, weight) in edges {
            if out_weight[*from] > 0.0 {
                next[*to] += DAMPING * ranks[*from] * weight / out_weight[*from];
            }
        }
        let change: f64 = next.iter().zip(&ranks).map(|(a, b)| (a - b).abs()).sum();
        ranks = next;
        if change < TOLERANCE {
            break;
        }
    }
    ranks
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn def(name: &str, line: u32) -> Tag {
        Tag {
            name: name.to_owned(),
            kind: TagKind::Definition,
            line,
            syntax: "function".to_owned(),
            signature: format!("function {name}() {{"),
        }
    }

    fn reference(name: &str) -> Tag {
        Tag {
            name: name.to_owned(),
            kind: TagKind::Reference,
            line: 1,
            syntax: "call".to_owned(),
            signature: String::new(),
        }
    }

    fn file(path: &str, tags: Vec<Tag>) -> SourceFile {
        SourceFile {
            path: path.to_owned(),
            tags: Arc::new(tags),
        }
    }

    /// `app` uses `useTasks` from `hooks` a lot and `formatDate` from `util`
    /// once; `admin` uses `auditLog` from `audit`; `README.md` has no tags.
    fn tree() -> Vec<SourceFile> {
        vec![
            file(
                "src/app.tsx",
                vec![
                    def("App", 1),
                    reference("useTasks"),
                    reference("useTasks"),
                    reference("useTasks"),
                    reference("formatDate"),
                ],
            ),
            file("src/hooks.ts", vec![def("useTasks", 4)]),
            file("src/util.ts", vec![def("formatDate", 2), def("unused", 9)]),
            file(
                "src/admin.tsx",
                vec![def("Admin", 1), reference("auditLog")],
            ),
            file("src/audit.ts", vec![def("auditLog", 3)]),
            file("README.md", vec![]),
        ]
    }

    fn names(ranked: &[Ranked]) -> Vec<String> {
        ranked
            .iter()
            .map(|r| match r {
                Ranked::Definition { tag, .. } => tag.name.clone(),
                Ranked::File { path } => path.clone(),
            })
            .collect()
    }

    #[test]
    fn pagerank_sums_to_one_and_follows_the_links() {
        let edges = vec![(0, 1, "x", 1.0), (2, 1, "x", 1.0), (1, 0, "y", 1.0)];
        let ranks = pagerank(3, &edges, &BTreeSet::new());
        assert!((ranks.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(ranks[1] > ranks[0] && ranks[0] > ranks[2], "{ranks:?}");
        // Personalised on node 2: it outranks what it is not linked to.
        let personal: BTreeSet<usize> = [2].into_iter().collect();
        let ranks = pagerank(3, &edges, &personal);
        assert!(ranks[2] > 0.1, "{ranks:?}");
    }

    #[test]
    fn a_focus_moves_its_files_definitions_first_and_what_they_use_up() {
        let files = tree();
        let unfocused = names(&rank(&files, &Focus::none()));
        // Every definition is there, then the file with none.
        assert_eq!(unfocused.len(), 7);
        assert_eq!(unfocused.last().map(String::as_str), Some("README.md"));

        let focus = Focus {
            files: ["src/admin.tsx".to_owned()].into_iter().collect(),
            names: BTreeSet::new(),
        };
        let focused = names(&rank(&files, &focus));
        assert_eq!(
            focused[0], "Admin",
            "the focus file's own definitions first"
        );
        assert_eq!(focused[1], "auditLog", "then what it uses: {focused:?}");

        let focus = Focus {
            files: ["src/app.tsx".to_owned()].into_iter().collect(),
            names: BTreeSet::new(),
        };
        let focused = names(&rank(&files, &focus));
        assert_eq!(
            &focused[..3],
            &["App", "useTasks", "formatDate"],
            "{focused:?}"
        );
    }

    #[test]
    fn a_named_symbol_outranks_its_peers() {
        let files = vec![
            file("a.ts", vec![reference("alpha"), reference("beta")]),
            file("b.ts", vec![def("alpha", 1)]),
            file("c.ts", vec![def("beta", 1)]),
        ];
        let plain = names(&rank(&files, &Focus::none()));
        let focus = Focus {
            files: BTreeSet::new(),
            names: ["beta".to_owned()].into_iter().collect(),
        };
        let named = names(&rank(&files, &focus));
        assert_eq!(plain[..2], ["alpha", "beta"], "ties break on path");
        assert_eq!(named[..2], ["beta", "alpha"]);
    }

    #[test]
    fn a_tree_ranks_the_same_way_twice() {
        assert_eq!(rank(&tree(), &Focus::none()), rank(&tree(), &Focus::none()));
    }

    #[test]
    fn terms_split_into_known_files_and_identifiers() {
        let known: BTreeSet<&str> = ["src/App.tsx"].into_iter().collect();
        let focus = Focus::from_terms(
            [
                "src/App.tsx",
                "./src/App.tsx",
                "useTasks",
                "not a name",
                "src/missing.ts",
                "9lives",
            ],
            &known,
        );
        assert_eq!(focus.files.into_iter().collect::<Vec<_>>(), ["src/App.tsx"]);
        assert_eq!(focus.names.into_iter().collect::<Vec<_>>(), ["useTasks"]);
    }
}
