//! An application's own strings: finding them, filling them, and saving them
//! (§16.x, task 4.4).
//!
//! This is the server half of the Translations screen. Three questions, and
//! each is answered by asking the application rather than by keeping a record
//! of it:
//!
//! 1. **What does this application say?** For an application with a project
//!    tree, [`application_strings`] runs `sc_i18n::extract` over its source
//!    through its own file store — the same tree-sitter pass `feldspar i18n
//!    extract` runs on a laptop (D6), so the screen and the command cannot
//!    disagree about what a project contains. It works on a project that has
//!    never been built, without npm and while an agent is halfway through
//!    editing it. For an application whose definition is rows (Saltcorn UI),
//!    the strings come from the views themselves.
//! 2. **How much of it is translated?** The catalogue for each enabled locale,
//!    through [`CatalogStore`](sc_app::CatalogStore), counted against those
//!    keys.
//! 3. **What is missing, and what is left over?** A key with no translation is
//!    *missing* and is what **Translate missing** fills. A key in the catalogue
//!    that the source no longer uses is an *orphan*: it is **shown and never
//!    deleted**, because the source may be mid-edit, because a key can be used
//!    from a file the extractor does not read, and because throwing away a
//!    human translation to tidy a list is not a trade anyone asked for.
//!
//! The [`Translator`] over a configured LLM ([`LlmTranslator`]) lives here for
//! the reason the extraction does: both callers of it — this screen and
//! `feldspar i18n translate` — are above this crate, and two implementations of
//! one prompt would drift on the first fix to either. D9 in one line: **the LLM
//! translates and the machine checks the placeholders**, which
//! `sc_i18n::translate_missing` does whatever the prompt said.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use sc_app::{AppId, Application, app_catalog_store, app_locales};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_files::FileStore;
use sc_i18n::extract::js::Language;
use sc_i18n::extract::{Extracted, Extraction, Finding, Problem};
use sc_i18n::{
    Catalog as MessageCatalog, Locale, Message, Translator, categories_for, source_text,
};
use serde_json::{Value as Json, json};

/// Directories a source scan never walks: a dependency tree, a build output, a
/// repository's own bookkeeping.
///
/// The same list `feldspar i18n` uses, and for the same reason: `node_modules`
/// alone is tens of thousands of files, none of them this application's
/// strings.
const SKIPPED_DIRECTORIES: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    ".git",
    ".vite",
    ".next",
    "target",
    "vendor",
];

/// Files that **define** the runtime rather than call it.
///
/// `i18n.tsx` and `messages.ts` are where `t`, `tc` and `<T>` are written, so
/// they hold the one `t(text, args)` in the tree whose message is a variable —
/// and they have to. Reported, it would be a permanent error on every screen,
/// which is a rule that teaches people to ignore the screen.
const RUNTIME_FILES: &[&str] = &["i18n.ts", "i18n.tsx", "messages.ts"];

/// How many files one scan will read before giving up.
///
/// A file store is somebody's repository and a scan is a request: a project
/// that has grown a directory nobody expected must make the screen slow, not
/// make the server unavailable. The cap is far above any real project's source
/// tree.
const MAX_FILES: usize = 4_000;

/// Everything one scan of an application found.
#[derive(Debug, Default)]
pub struct AppStrings {
    /// Every `t(…)` / `tc(…)` / `<T text="…">` call site.
    pub extraction: Extraction,
    /// Bare English literals nothing wraps — what `feldspar i18n lint` reports.
    pub findings: Vec<Finding>,
    /// How many source files were read, for the line the screen shows.
    pub files: usize,
    /// Set when the scan stopped at [`MAX_FILES`], so the screen can say the
    /// list is not the whole story rather than quietly under-reporting.
    pub truncated: bool,
}

impl AppStrings {
    /// The keys, de-duplicated and sorted — what coverage is counted against.
    pub fn keys(&self) -> Vec<String> {
        self.extraction.keys()
    }
}

/// Everything an application says, whichever half of the split it is on.
///
/// A code application's strings are `t()` call sites a parser finds; a
/// Saltcorn UI application's are values in its views' configurations that only
/// each pattern can read. Both answer the same [`AppStrings`], which is what
/// lets the Translations screen, the coverage figure and **Translate missing**
/// be written once.
pub async fn application_strings(cat: &Catalog, app: &Application) -> Result<AppStrings> {
    if source_tree(cat, app)?.is_none() {
        return view_strings(cat, app).await;
    }
    source_strings(cat, app).await
}

/// Extract and lint an application's source, through the application's own file
/// store.
async fn source_strings(cat: &Catalog, app: &Application) -> Result<AppStrings> {
    let Some((store, project)) = source_tree(cat, app)? else {
        return Ok(AppStrings::default());
    };
    let mut paths = Vec::new();
    walk(store.as_ref(), &project, &mut paths).await?;
    let truncated = paths.len() > MAX_FILES;
    paths.truncate(MAX_FILES);

    let mut out = AppStrings {
        truncated,
        ..AppStrings::default()
    };
    for path in paths {
        let Some(language) = Language::for_path(&path) else {
            continue;
        };
        let Ok(bytes) = store.read(&path).await else {
            // A file that vanished between the listing and the read is a
            // project being edited, which is the normal state of one.
            continue;
        };
        // Relative to the project, so the screen's file column reads the way
        // the developer's editor does.
        let name = path
            .strip_prefix(&project)
            .unwrap_or(&path)
            .trim_start_matches('/')
            .to_owned();
        let (found, lint) = sc_i18n::extract::js::scan(language, &name, &bytes);
        out.extraction.merge(found);
        out.findings.extend(lint);
        out.files += 1;
    }
    out.findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.text.cmp(&b.text))
    });
    Ok(out)
}

/// A Saltcorn UI application's strings: what each of its views' configurations
/// puts in front of a person, through the pattern's own `getStringsForI18n`
/// (task 4.5).
///
/// There is no lint here and there are no unreadable call sites, because there
/// is no source to parse: an admin configuring a view types the header of a
/// column into a form, and every such value the pattern names is a message.
/// The "file" a message is attributed to is the view it was configured on,
/// which is where an admin would go to change it.
async fn view_strings(cat: &Catalog, app: &Application) -> Result<AppStrings> {
    // A server built without the Saltcorn UI bundle has no runtime, and an
    // application whose framework is not a views-and-pages one has no views.
    // Neither is a failure of this screen: the answer is that it says nothing.
    let Ok(runtime) = sc_viewpattern::view_runtime() else {
        return Ok(AppStrings::default());
    };
    let views = sc_viewpattern::list_views(cat, app.id).await?;
    if views.is_empty() {
        return Ok(AppStrings::default());
    }
    let configurer = sc_viewpattern::Configurer::new(runtime, cat, app, None, None).await?;
    let mut out = AppStrings::default();
    for view in &views {
        let strings = configurer.strings_for_i18n(view).await?;
        for text in strings {
            out.extraction.messages.push(Extracted {
                key: text,
                file: view.name.clone(),
                line: 0,
            });
        }
        out.files += 1;
    }
    Ok(out)
}

/// The file store and project directory an application's source is in, or
/// `None` for an application that has no tree.
fn source_tree(
    cat: &Catalog,
    app: &Application,
) -> Result<Option<(std::sync::Arc<dyn FileStore>, String)>> {
    let Ok(source) = sc_app::app_source_from_config(&app.framework) else {
        return Ok(None);
    };
    let store = cat.require_file_store(&source.store.0)?;
    Ok(Some((store, source.build.source_dir)))
}

/// Every candidate source file under `dir`, sorted, so two scans of an
/// unchanged tree answer in the same order.
async fn walk(store: &dyn FileStore, dir: &str, out: &mut Vec<String>) -> Result<()> {
    // Breadth-first with an explicit queue rather than recursion, because an
    // `async fn` cannot recurse without boxing and a directory tree is not a
    // reason to allocate a future per level.
    let mut queue = vec![dir.to_owned()];
    while let Some(current) = queue.pop() {
        let Ok(mut entries) = store.list(&current).await else {
            continue;
        };
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for entry in entries {
            if entry.is_dir {
                if !SKIPPED_DIRECTORIES.contains(&entry.name.as_str())
                    && !entry.name.starts_with('.')
                {
                    queue.push(entry.path);
                }
                continue;
            }
            if scannable(&entry.name) {
                out.push(entry.path);
            }
        }
        if out.len() > MAX_FILES {
            break;
        }
    }
    out.sort();
    Ok(())
}

fn scannable(name: &str) -> bool {
    Language::for_path(name).is_some()
        && !name.contains(".test.")
        && !name.contains(".spec.")
        && !RUNTIME_FILES.contains(&name)
}

/// One catalogue's standing against a set of keys.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// Keys the source uses that this catalogue has a translation for.
    pub translated: usize,
    /// Keys the source uses, translated or not.
    pub total: usize,
    /// Keys in the catalogue the source no longer uses. Shown, never deleted.
    pub orphans: Vec<String>,
}

impl Coverage {
    /// Percent translated, rounded down. A domain with no messages is 100%
    /// translated, which is both arithmetically awkward and obviously right.
    pub fn percent(&self) -> u32 {
        if self.total == 0 {
            return 100;
        }
        ((self.translated * 100) / self.total) as u32
    }
}

/// How `catalog` stands against `keys`.
pub fn coverage(catalog: &MessageCatalog, keys: &[String]) -> Coverage {
    let wanted: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
    Coverage {
        translated: wanted.iter().filter(|k| catalog.get(k).is_some()).count(),
        total: wanted.len(),
        orphans: catalog.orphans(wanted.iter().copied()),
    }
}

// ---------------------------------------------------------------------------
// The screen's payload
// ---------------------------------------------------------------------------

/// The Translations screen, as JSON: the locales, the grid, the coverage, the
/// lint findings and the unreadable call sites.
///
/// One call rather than five, because the screen is one table and every column
/// of it is a function of the same scan: splitting it would make the browser
/// re-ask for a tree walk it has already paid for.
pub async fn translations_json(cat: &Catalog, app: &Application) -> Result<Json> {
    let strings = application_strings(cat, app).await?;
    let keys = strings.keys();
    let locales = app_locales(app)?;
    let store = app_catalog_store(app)?;

    let mut catalogues: BTreeMap<String, MessageCatalog> = BTreeMap::new();
    for locale in &locales {
        if let Some(loaded) = store.load(cat, locale).await? {
            catalogues.insert(locale.as_str().to_owned(), loaded);
        }
    }

    let rows: Vec<Json> = keys
        .iter()
        .map(|key| {
            let mut translations = serde_json::Map::new();
            for (tag, catalogue) in &catalogues {
                if let Some(message) = catalogue.get(key) {
                    translations.insert(tag.clone(), message.to_json());
                }
            }
            json!({
                "key": key,
                "source": source_text(key),
                "context": sc_i18n::key_context(key),
                "sites": sites_json(&strings.extraction, key),
                "translations": Json::Object(translations),
            })
        })
        .collect();

    let coverages: Vec<Json> = locales
        .iter()
        .map(|locale| {
            let tag = locale.as_str();
            let cover = catalogues
                .get(tag)
                .map(|c| coverage(c, &keys))
                .unwrap_or(Coverage {
                    translated: 0,
                    total: keys.len(),
                    orphans: Vec::new(),
                });
            json!({
                "locale": tag,
                "direction": locale.direction().as_str(),
                "translated": cover.translated,
                "total": cover.total,
                "percent": cover.percent(),
                "orphans": cover.orphans,
            })
        })
        .collect();

    Ok(json!({
        "store": store.describe(),
        "scanned": strings.files,
        "truncated": strings.truncated,
        "default_locale": sc_app::app_default_locale(app)?.map(|l| l.as_str().to_owned()),
        "locales": coverages,
        "messages": rows,
        // A call site whose message is not a literal: it can never reach a
        // catalogue, so it is an error rather than a warning (task 2.1).
        "problems": strings
            .extraction
            .problems
            .iter()
            .map(problem_json)
            .collect::<Vec<_>>(),
        // The opposite: a literal nobody wrapped.
        "unwrapped": strings.findings.iter().map(finding_json).collect::<Vec<_>>(),
    }))
}

fn sites_json(extraction: &Extraction, key: &str) -> Json {
    let sites: Vec<Json> = extraction
        .messages
        .iter()
        .filter(|m: &&Extracted| m.key == key)
        .map(|m| json!({ "file": m.file, "line": m.line }))
        .collect();
    Json::Array(sites)
}

fn problem_json(problem: &Problem) -> Json {
    json!({
        "file": problem.file,
        "line": problem.line,
        "message": problem.message,
    })
}

fn finding_json(finding: &Finding) -> Json {
    json!({
        "file": finding.file,
        "line": finding.line,
        "text": finding.text,
        "message": finding.message(),
    })
}

// ---------------------------------------------------------------------------
// Saving and filling
// ---------------------------------------------------------------------------

/// Replace `locale`'s catalogue with `messages`, and drop the mount's cache so
/// the running application serves it on the next reload (D7).
///
/// Every value is checked before anything is written: a translation whose
/// placeholders or plural categories differ from its key's is refused naming
/// the key, exactly as a translation the LLM returned is. The admin typing one
/// by hand gets the same guarantee the machine does — and a screen that told
/// them at save time is better than an application that renders `{name}` to a
/// customer.
pub async fn save_catalogue(
    cat: &Catalog,
    apps: &crate::apps::AppMounts,
    app: &Application,
    locale: &Locale,
    messages: &Json,
) -> Result<Json> {
    if !app_locales(app)?
        .iter()
        .any(|l| l.as_str() == locale.as_str())
    {
        return Err(Error::invalid(format!(
            "application `{}` does not serve `{}`; enable the locale before \
             translating into it",
            app.name,
            locale.as_str()
        )));
    }
    let mut catalogue = MessageCatalog::from_json(locale.clone(), messages)?;
    let mut rejected = Vec::new();
    for (key, message) in catalogue.messages() {
        if let Err(why) = sc_i18n::check(key, message, locale) {
            // The key, then the reason. A screen showing "the translation uses
            // {nom} where the source uses {name}" with no key names the
            // mistake and not the cell.
            rejected.push(format!("`{key}`: {why}"));
        }
    }
    if !rejected.is_empty() {
        return Err(Error::invalid(rejected.join("; ")));
    }

    // **Orphans are put back here, not by the caller.** A key the source no
    // longer uses never appears in the grid, so a save from the grid cannot
    // have been asked to delete one — and a body that simply omits it would
    // throw away a human's translation the first time somebody renamed a
    // string. Which keys those are is the same question coverage asks, so it
    // is asked the same way: anything the catalogue already had that the
    // source does not say.
    let store = app_catalog_store(app)?;
    if let Some(existing) = store.load(cat, locale).await? {
        let used: BTreeSet<String> = application_strings(cat, app)
            .await?
            .keys()
            .into_iter()
            .collect();
        for (key, message) in existing.messages() {
            if !used.contains(key) && catalogue.get(key).is_none() {
                catalogue.insert(key.clone(), message.clone());
            }
        }
    }
    store.save(cat, &catalogue).await?;
    apps.invalidate_catalogs(app.id);
    Ok(json!({ "locale": locale.as_str(), "messages": catalogue.len() }))
}

/// Fill everything `locale` has not got, through `translator`, and save the
/// result.
///
/// The keys are the ones the source actually uses — an orphan is not
/// re-translated, because nothing renders it. The report names every message
/// the check rejected, so "the LLM renamed a placeholder" is a line on the
/// screen rather than a string that renders wrongly to a customer.
pub async fn fill_missing(
    cat: &Catalog,
    apps: &crate::apps::AppMounts,
    app: &Application,
    locale: &Locale,
    translator: &dyn Translator,
) -> Result<Json> {
    if !app_locales(app)?
        .iter()
        .any(|l| l.as_str() == locale.as_str())
    {
        return Err(Error::invalid(format!(
            "application `{}` does not serve `{}`",
            app.name,
            locale.as_str()
        )));
    }
    let keys = application_strings(cat, app).await?.keys();
    let store = app_catalog_store(app)?;
    let mut catalogue = store
        .load(cat, locale)
        .await?
        .unwrap_or_else(|| MessageCatalog::new(locale.clone()));
    let missing = catalogue.missing(keys.iter().map(String::as_str));
    if missing.is_empty() {
        return Ok(json!({
            "locale": locale.as_str(),
            "filled": 0,
            "rejected": [],
            "warnings": [],
        }));
    }
    let report =
        sc_i18n::translate_missing(&mut catalogue, &Locale::source(), translator, &missing).await?;
    store.save(cat, &catalogue).await?;
    apps.invalidate_catalogs(app.id);
    Ok(json!({
        "locale": locale.as_str(),
        // A count, because the screen shows a sentence; the keys are beside it
        // for the admin who wants to know which.
        "filled": report.filled.len(),
        "keys": report.filled,
        "unanswered": report.unanswered,
        "rejected": report
            .rejected
            .iter()
            .map(|r| json!({ "key": r.key, "reason": r.reason }))
            .collect::<Vec<_>>(),
        "warnings": report.warnings(),
    }))
}

/// Which locales an application serves, written to its record.
///
/// Turning a locale *off* does not delete its catalogue: the file stays in the
/// repository and the row stays in the table, so turning it back on is free and
/// a mis-click costs nothing. What it does is stop the catalogue being served
/// and take the locale off the picker.
pub async fn set_locales(
    cat: &Catalog,
    apps: &crate::apps::AppMounts,
    id: AppId,
    tags: &[String],
    default: Option<&str>,
) -> Result<Json> {
    let mut app = sc_app::load_application(cat, id)
        .await?
        .ok_or_else(|| Error::not_found(format!("no application with id {id}")))?;
    let locales: Vec<Locale> = tags
        .iter()
        .map(|t| Locale::parse(t))
        .collect::<Result<_>>()?;
    let default = match default {
        Some(tag) => Some(Locale::parse(tag)?),
        None => None,
    };
    if let Some(default) = &default
        && !locales.iter().any(|l| l.as_str() == default.as_str())
    {
        return Err(Error::invalid(format!(
            "`{}` is the default locale but is not one of the enabled locales",
            default.as_str()
        )));
    }
    sc_app::set_app_locales(&mut app, &locales, default.as_ref());
    let saved = sc_app::save_application(cat, &app).await?;
    // The running mount holds a *copy* of the record, and the locale set is
    // read off it on every request — by the router when it negotiates, and by a
    // server-rendered framework as it renders. Saving the row without this
    // would turn French on everywhere except in the application.
    apps.refresh_mount(saved.clone()).await?;
    apps.invalidate_catalogs(saved.id);
    Ok(json!({
        "locales": locales.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
        "default_locale": default.map(|l| l.as_str().to_owned()),
    }))
}

// ---------------------------------------------------------------------------
// The translator over a configured LLM provider (decision D9).
// ---------------------------------------------------------------------------

/// [`Translator`] over one configured model.
///
/// The prompt states the format and the required plural categories, and then
/// **the machine checks the answer anyway** — `translate_missing` rejects a
/// message whose placeholders drifted, whatever the prompt said. That is the
/// whole of D9: a prompt is a request and a check is a guarantee.
pub struct LlmTranslator {
    model: sc_llm::ConnectedModel,
}

impl LlmTranslator {
    /// A translator over `model`.
    pub fn new(model: sc_llm::ConnectedModel) -> LlmTranslator {
        LlmTranslator { model }
    }

    /// What the model is called, for the line a command prints and the sentence
    /// the screen shows.
    pub fn describes(&self) -> String {
        format!(
            "{}/{}",
            self.model.provider_name,
            self.model.provider.model()
        )
    }
}

#[async_trait]
impl Translator for LlmTranslator {
    async fn translate_batch(
        &self,
        source: &Locale,
        target: &Locale,
        keys: &[String],
    ) -> Result<BTreeMap<String, Json>> {
        let categories: Vec<&str> = categories_for(target).iter().map(|c| c.as_str()).collect();
        let system = format!(
            "You are translating a software product's user interface from {} into {}.\n\
             \n\
             You are given a JSON array of source strings. Answer with a JSON object and \
             nothing else: every key is one of the given strings, copied **exactly**, and \
             its value is the translation.\n\
             \n\
             Rules:\n\
             - `{{name}}` is a placeholder the program fills in. Keep every placeholder, \
               spelled exactly as in the source. Never translate, rename, add or drop one. \
               `{{{{` is a literal opening brace.\n\
             - A key containing the character U+0004 is `context\\u0004text`: the part \
               before it is a disambiguating hint for you (`verb`, `noun`, a screen name), \
               and only the part after it is to be translated. Answer under the whole key, \
               separator included.\n\
             - If the source contains `{{count}}`, answer with an object of plural forms \
               instead of a string, with exactly these keys: {}. Otherwise answer with a \
               string.\n\
             - Keep the register and the capitalisation conventions of {}. These are button \
               labels, field labels and short sentences in an application, not prose.\n\
             - The text is never HTML and must not be escaped as if it were.\n\
             - If you cannot translate a string, leave it out of the object rather than \
               guessing.",
            source.as_str(),
            target.as_str(),
            categories.join(", "),
            target.as_str(),
        );
        let prompt = serde_json::to_string_pretty(&keys)
            .map_err(|e| Error::invalid(format!("building the translation request: {e}")))?;
        let request = sc_llm::LlmRequest::prompt(prompt)
            .system(system)
            .temperature(0.0);
        let answer = self.model.provider.stream(request).await?.collect().await?;
        parse_answer(&answer.content)
    }
}

/// The model's reply, as an object of key to translation.
///
/// Tolerant of a fenced code block and of prose either side of the object,
/// because every provider does one of those occasionally and failing the whole
/// batch over a pair of backticks would throw away fifty good translations.
pub fn parse_answer(text: &str) -> Result<BTreeMap<String, Json>> {
    let trimmed = text.trim();
    let body = match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => {
            return Err(Error::invalid(format!(
                "the translator answered with no JSON object: {}",
                trimmed.chars().take(200).collect::<String>()
            )));
        }
    };
    let parsed: Json = serde_json::from_str(body)
        .map_err(|e| Error::invalid(format!("the translator's answer is not JSON: {e}")))?;
    match parsed {
        Json::Object(map) => Ok(map.into_iter().collect()),
        other => Err(Error::invalid(format!(
            "the translator answered with {} rather than an object",
            match other {
                Json::Array(_) => "an array",
                _ => "a scalar",
            }
        ))),
    }
}

/// The translator this installation is configured with: a named LLM provider,
/// or the only sensible default — the first one configured, with its default
/// model.
///
/// The error says what to configure rather than what failed, because "no LLM
/// provider" is not a fault, it is a step the admin has not taken yet.
pub async fn configured_translator(
    cat: &Catalog,
    provider_name: Option<&str>,
) -> Result<LlmTranslator> {
    let provider = match provider_name {
        Some(name) => sc_llm::load_llm_provider_by_name(cat, name)
            .await?
            .ok_or_else(|| Error::not_found(format!("no LLM provider named `{name}`")))?,
        None => sc_llm::list_llm_providers(cat)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                Error::config(
                    "no LLM provider is configured — add one in Settings → LLM providers \
                     before translating",
                )
            })?,
    };
    let model = sc_llm::require_llm_model(cat, &provider, None).await?;
    Ok(LlmTranslator::new(sc_llm::connect_model(
        &provider, &model,
    )?))
}

/// A one-line summary of what a catalogue holds for a key.
pub fn preview(message: &Message) -> String {
    message.forms().join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_skips_tests_and_the_runtime_that_defines_t() {
        assert!(scannable("App.tsx"));
        assert!(scannable("pages/Tasks.tsx"));
        assert!(!scannable("App.test.tsx"));
        assert!(!scannable("App.spec.ts"));
        // The two files where `t` itself is written: the one `t(text, args)`
        // whose message is a variable has to be there.
        assert!(!scannable("i18n.tsx"));
        assert!(!scannable("messages.ts"));
        // Not source at all.
        assert!(!scannable("README.md"));
        assert!(!scannable("schema.sql"));
    }

    #[test]
    fn coverage_counts_what_the_source_uses_and_names_what_it_does_not() {
        let fr = Locale::parse("fr").unwrap();
        let mut catalogue = MessageCatalog::new(fr);
        catalogue.insert("Add", Message::Simple("Ajouter".to_owned()));
        catalogue.insert("Gone", Message::Simple("Parti".to_owned()));

        let keys = vec!["Add".to_owned(), "Delete".to_owned()];
        let cover = coverage(&catalogue, &keys);
        assert_eq!((cover.translated, cover.total), (1, 2));
        assert_eq!(cover.percent(), 50);
        // Translated, but nothing renders it: shown, never deleted.
        assert_eq!(cover.orphans, vec!["Gone".to_owned()]);

        // An application with nothing to translate is translated.
        assert_eq!(coverage(&catalogue, &[]).percent(), 100);
    }

    #[test]
    fn an_answer_survives_a_fenced_block_and_refuses_what_is_not_an_object() {
        let parsed = parse_answer("```json\n{\"Add\": \"Ajouter\"}\n```").unwrap();
        assert_eq!(parsed["Add"], json!("Ajouter"));
        assert!(parse_answer("[\"Ajouter\"]").is_err());
        assert!(parse_answer("I cannot help with that").is_err());
    }
}
