//! `saltcorn api` — an application's **custom SQL queries** from the command
//! line (§13.4).
//!
//! The admin UI can add one; so can this, and for the reasons `build-app`
//! exists: it is scriptable, it is what a deploy step or a coding agent calls,
//! and it works when there is no browser pointed at the server. The generated
//! `src/saltcorn/README.md` tells an agent working in an app's project to use it,
//! which is the same reason there are three commands rather than one — an
//! add-only command is a trap, because the first typo would need a browser to
//! fix, which is exactly the situation the command exists to avoid.
//!
//! This module is the **parsing** and the **selection**: turning the flags into
//! a [`CustomQuery`] and finding the API row that will hold it. Connecting,
//! saving and re-emitting the client are the binary's, so this half is testable
//! without a database.

use sc_api::{CustomParam, CustomQuery, Method, ValueType};
use sc_app::{ApiConfig, Application, registered_api_provider_info};
use sc_error::{Error, Result};

/// `saltcorn api add-query`'s arguments, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct AddQueryArgs {
    /// `--app`: which application, by subdomain.
    pub app: String,
    /// `--api`: which of its APIs, by mount. Optional when only one of them
    /// serves custom queries.
    pub api: Option<String>,
    /// The query itself, as the flags describe it.
    pub query: CustomQuery,
}

/// `saltcorn api list-queries` / `remove-query`'s arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryRefArgs {
    /// `--app`: which application, by subdomain.
    pub app: String,
    /// `--api`: which of its APIs, by mount.
    pub api: Option<String>,
    /// `--name`: which query. Required by `remove-query`, unused by
    /// `list-queries`.
    pub name: Option<String>,
}

/// Parse `add-query`'s flags.
///
/// ```text
/// --app SUBDOMAIN [--api MOUNT] --name NAME [--method GET] --path /sub/path
/// [--min-role N] [--description TEXT] [--param name:type[,name:type…]]… --sql TEXT|@FILE
/// ```
///
/// The defaults are the model's own: `GET`, and the **admin** role floor. An
/// unstated floor is admin because that is what an unstated one means everywhere
/// else a query is written (§10.2) — a command-line default of "public" would be
/// the one place the rule did not hold.
pub fn parse_add_query(args: &[String]) -> Result<AddQueryArgs> {
    let mut app = None;
    let mut api = None;
    let mut name = None;
    let mut method = None;
    let mut path = None;
    let mut min_role = None;
    let mut description = None;
    let mut sql = None;
    let mut params: Vec<CustomParam> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--app" => app = Some(value(&mut it, arg)?),
            "--api" => api = Some(value(&mut it, arg)?),
            "--name" => name = Some(value(&mut it, arg)?),
            "--method" => method = Some(value(&mut it, arg)?),
            "--path" => path = Some(value(&mut it, arg)?),
            "--min-role" => min_role = Some(value(&mut it, arg)?),
            "--description" => description = Some(value(&mut it, arg)?),
            "--sql" => sql = Some(value(&mut it, arg)?),
            "--param" => {
                for spec in value(&mut it, arg)?.split(',') {
                    params.push(parse_param(spec.trim())?);
                }
            }
            other => {
                return Err(Error::config(format!(
                    "unknown add-query argument `{other}`"
                )));
            }
        }
    }

    let app = required(app, "--app")?;
    let name = required(name, "--name")?;
    let path = required(path, "--path")?;
    let sql = read_sql(&required(sql, "--sql")?)?;
    let method = match method {
        Some(m) => parse_method(&m)?,
        None => Method::Get,
    };
    let min_role = match min_role {
        Some(r) => r.parse::<u8>().map_err(|_| {
            Error::config(format!("--min-role wants a number in 1..=100, not `{r}`"))
        })?,
        None => sc_auth::ROLE_ADMIN,
    };

    let mut query = CustomQuery::new(name, method, path, sql)
        .params(params)
        .min_role(min_role);
    if let Some(description) = description {
        query = query.description(description);
    }
    Ok(AddQueryArgs { app, api, query })
}

/// Parse the `--app`/`--api`/`--name` flags `list-queries` and `remove-query`
/// share.
pub fn parse_query_ref(command: &str, args: &[String]) -> Result<QueryRefArgs> {
    let mut app = None;
    let mut api = None;
    let mut name = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--app" => app = Some(value(&mut it, arg)?),
            "--api" => api = Some(value(&mut it, arg)?),
            "--name" => name = Some(value(&mut it, arg)?),
            other => {
                return Err(Error::config(format!(
                    "unknown {command} argument `{other}`"
                )));
            }
        }
    }
    Ok(QueryRefArgs {
        app: required(app, "--app")?,
        api,
        name,
    })
}

/// One `--param name:type` or `name:type?` (the `?` marking it optional).
///
/// An optional parameter binds SQL `NULL` when the caller leaves it out, which
/// is what makes `(:q is null or name = :q)` the idiom for an optional filter —
/// so the spelling that says "may be omitted" is worth having on the command
/// line too.
fn parse_param(spec: &str) -> Result<CustomParam> {
    let (name, ty) = spec.split_once(':').ok_or_else(|| {
        Error::config(format!(
            "--param wants `name:type` (e.g. `since:int`, or `q:text?` for an \
             optional one), not `{spec}`"
        ))
    })?;
    let (ty, required) = match ty.strip_suffix('?') {
        Some(ty) => (ty, false),
        None => (ty, true),
    };
    let ty = ValueType::from_name(ty.trim()).ok_or_else(|| {
        Error::config(format!(
            "`{ty}` is not a parameter type; use one of {}",
            ValueType::ALL
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    let param = CustomParam::new(name.trim(), ty);
    Ok(if required { param } else { param.optional() })
}

/// `--sql`'s value: the SQL itself, or `@path` naming a file holding it.
///
/// A file, because SQL is many lines and a shell is a poor place to keep them —
/// the query an admin actually wants is usually already in a `.sql` file beside
/// the project, and `--sql @report.sql` is how it gets in without a heredoc.
fn read_sql(value: &str) -> Result<String> {
    let Some(path) = value.strip_prefix('@') else {
        return Ok(value.to_owned());
    };
    std::fs::read_to_string(path)
        .map_err(|e| Error::config(format!("cannot read the SQL file `{path}`: {e}")))
}

/// `--method`'s value, as the model's enum.
fn parse_method(raw: &str) -> Result<Method> {
    match raw.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::Get),
        "POST" => Ok(Method::Post),
        "PUT" => Ok(Method::Put),
        "PATCH" => Ok(Method::Patch),
        "DELETE" => Ok(Method::Delete),
        other => Err(Error::config(format!(
            "`{other}` is not an HTTP method; use GET, POST, PUT, PATCH or DELETE"
        ))),
    }
}

/// The API row of `app` that holds custom queries, chosen by `--api`'s mount or,
/// when there is exactly one candidate, by there being nothing to choose.
///
/// Which providers are candidates comes from
/// [`registered_api_provider_info`]'s `supports_custom_queries` — the same
/// declaration the admin form offers the editor on — so a provider that grows
/// custom queries is offered here too without this file hearing about it.
///
/// Ambiguity is refused rather than resolved: an application with two such APIs
/// has two places a query could land, and picking one would be picking which
/// client method appears where.
pub fn select_api<'a>(app: &'a mut Application, mount: Option<&str>) -> Result<&'a mut ApiConfig> {
    let mounts: Vec<String> = app.apis.iter().map(|a| a.mount.clone()).collect();
    if let Some(mount) = mount {
        return app
            .apis
            .iter_mut()
            .find(|a| a.mount == mount)
            .ok_or_else(|| {
                Error::config(format!(
                    "application `{}` has no API mounted at `{mount}`; it has {}",
                    app.subdomain,
                    list(&mounts)
                ))
            });
    }
    let candidates: Vec<usize> = app
        .apis
        .iter()
        .enumerate()
        .filter(|(_, a)| serves_custom_queries(&a.provider))
        .map(|(i, _)| i)
        .collect();
    match candidates.as_slice() {
        [only] => Ok(&mut app.apis[*only]),
        [] => Err(Error::config(format!(
            "application `{}` has no API that serves custom SQL queries; it has {}",
            app.subdomain,
            list(&mounts)
        ))),
        _ => Err(Error::config(format!(
            "application `{}` has more than one API that serves custom SQL \
             queries, so name one with --api: {}",
            app.subdomain,
            list(
                &candidates
                    .iter()
                    .map(|i| app.apis[*i].mount.clone())
                    .collect::<Vec<_>>()
            )
        ))),
    }
}

/// Whether the provider registered under `name` serves custom SQL queries.
pub fn serves_custom_queries(name: &str) -> bool {
    registered_api_provider_info()
        .iter()
        .any(|p| p.name == name && p.supports_custom_queries)
}

/// `` `a`, `b` `` — or "none" for an empty list, so a message never trails off.
fn list(items: &[String]) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items
        .iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String> {
    it.next()
        .cloned()
        .ok_or_else(|| Error::config(format!("{flag} requires a value")))
}

fn required(value: Option<String>, flag: &str) -> Result<String> {
    value.ok_or_else(|| Error::config(format!("{flag} is required")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn base() -> Vec<String> {
        args(&[
            "--app",
            "blog",
            "--name",
            "topAuthors",
            "--path",
            "/reports/top-authors",
            "--sql",
            "select author from books where year > :since",
            "--param",
            "since:int",
        ])
    }

    #[test]
    fn it_parses_a_query_and_defaults_to_get_at_the_admin_floor() {
        let parsed = parse_add_query(&base()).unwrap();
        assert_eq!(parsed.app, "blog");
        assert_eq!(parsed.api, None);
        assert_eq!(parsed.query.name, "topAuthors");
        assert_eq!(parsed.query.method, Method::Get);
        assert_eq!(parsed.query.path, "/reports/top-authors");
        assert_eq!(parsed.query.min_role, sc_auth::ROLE_ADMIN);
        assert_eq!(
            parsed.query.params,
            vec![CustomParam::new("since", ValueType::Int)]
        );
    }

    #[test]
    fn several_parameters_may_share_one_flag_or_take_one_each() {
        let mut one = base();
        one.extend(args(&["--param", "q:text?,limit:int"]));
        let parsed = parse_add_query(&one).unwrap();
        assert_eq!(
            parsed.query.params,
            vec![
                CustomParam::new("since", ValueType::Int),
                // The `?` is what says "may be omitted", and an omitted one binds
                // NULL rather than being refused.
                CustomParam::new("q", ValueType::Text).optional(),
                CustomParam::new("limit", ValueType::Int),
            ]
        );
    }

    #[test]
    fn a_parameter_type_that_is_not_one_is_refused_naming_the_choices() {
        let mut bad = base();
        bad.extend(args(&["--param", "n:integer"]));
        let msg = parse_add_query(&bad).unwrap_err().to_string();
        assert!(msg.contains("integer"), "{msg}");
        assert!(msg.contains("int,"), "{msg}");

        let mut worse = base();
        worse.extend(args(&["--param", "n"]));
        assert!(parse_add_query(&worse).is_err());
    }

    #[test]
    fn the_sql_may_come_from_a_file() {
        let dir = std::env::temp_dir().join(format!("sc-cli-api-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.sql");
        std::fs::write(&path, "select 1 as n").unwrap();

        let mut with_file = args(&["--app", "blog", "--name", "r", "--path", "/r"]);
        with_file.extend(args(&["--sql", &format!("@{}", path.display())]));
        assert_eq!(
            parse_add_query(&with_file).unwrap().query.sql,
            "select 1 as n"
        );

        // …and a file that is not there is a *config* error naming it, not a
        // query saved with the literal text `@/tmp/nope.sql`.
        let mut missing = args(&["--app", "blog", "--name", "r", "--path", "/r"]);
        missing.extend(args(&["--sql", "@/nonexistent/nope.sql"]));
        let msg = parse_add_query(&missing).unwrap_err().to_string();
        assert!(msg.contains("nope.sql"), "{msg}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_required_flag_and_an_unknown_one_are_both_refused() {
        for missing in ["--app", "--name", "--path", "--sql"] {
            let kept: Vec<String> = {
                let all = base();
                let mut out = Vec::new();
                let mut it = all.into_iter();
                while let Some(a) = it.next() {
                    if a == missing {
                        it.next();
                        continue;
                    }
                    out.push(a);
                }
                out
            };
            let msg = parse_add_query(&kept).unwrap_err().to_string();
            assert!(msg.contains(missing), "{missing}: {msg}");
        }
        let mut unknown = base();
        unknown.extend(args(&["--role", "40"]));
        let msg = parse_add_query(&unknown).unwrap_err().to_string();
        assert!(msg.contains("--role"), "{msg}");

        // A flag whose value is missing is not a flag with an empty value.
        assert!(parse_add_query(&args(&["--app"])).is_err());
    }

    #[test]
    fn the_method_and_the_role_floor_are_the_admins_choice() {
        let mut chosen = base();
        chosen.extend(args(&["--method", "post", "--min-role", "40"]));
        let parsed = parse_add_query(&chosen).unwrap();
        assert_eq!(parsed.query.method, Method::Post);
        assert_eq!(parsed.query.min_role, 40);

        let mut bad = base();
        bad.extend(args(&["--method", "FETCH"]));
        assert!(parse_add_query(&bad).is_err());
        let mut worse = base();
        worse.extend(args(&["--min-role", "everyone"]));
        assert!(parse_add_query(&worse).is_err());
    }

    #[test]
    fn list_queries_takes_the_app_and_an_optional_api() {
        let parsed = parse_query_ref("list-queries", &args(&["--app", "blog"])).unwrap();
        assert_eq!(parsed.app, "blog");
        assert_eq!(parsed.api, None);
        assert!(parse_query_ref("list-queries", &args(&["--api", "/api"])).is_err());
    }

    #[test]
    fn the_api_is_chosen_by_mount_or_by_there_being_one_that_serves_queries() {
        let mut app = Application::new("Blog", "blog", sc_app::FrameworkRef::new("react"))
            .with_api(ApiConfig::new("rest", "/api"))
            .with_api(ApiConfig::new("graphql", "/graphql"));

        // No `--api`: the REST one, because it is the only one that serves them.
        assert_eq!(select_api(&mut app, None).unwrap().mount, "/api");
        assert_eq!(
            select_api(&mut app, Some("/graphql")).unwrap().mount,
            "/graphql"
        );

        // A mount the app does not have names the ones it does.
        let msg = select_api(&mut app, Some("/v2")).unwrap_err().to_string();
        assert!(msg.contains("/api") && msg.contains("/graphql"), "{msg}");

        // Two that serve them is ambiguous, and picking one would be picking
        // which client method appears where.
        app.apis.push(ApiConfig::new("rest", "/api2"));
        let msg = select_api(&mut app, None).unwrap_err().to_string();
        assert!(msg.contains("--api"), "{msg}");

        // …and an app with none says so rather than inventing a place to put it.
        let mut none = Application::new("Blog", "blog", sc_app::FrameworkRef::new("react"));
        assert!(select_api(&mut none, None).is_err());
    }
}
