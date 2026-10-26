//! The **A/B tests workspace** (TODO-astro W3.6): the report on an Astro
//! site's landing-page experiments, made in the Analytics UI rather than in an
//! admin screen of its own.
//!
//! The Astro module (`plugins/astro`) declares two tables and its runtime
//! writes them: `astro_exposures` (one row per visitor and experiment: the
//! variant they were shown, and whether they have converted since) and
//! `astro_conversions` (one row per goal a visitor reached, for each
//! experiment they had seen). Both are kept per site, the application's name.
//!
//! [`ensure_ab_workspace`] makes, for one site, two datasets over those tables
//! filtered to it and a dashboard over the datasets: a sentence on how to read
//! it, visitors and conversion rate by experiment and variant, and conversions
//! by goal. Everything is an ordinary dataset and an ordinary dashboard, so the
//! admin can add a panel, a test or a plot to it like to any other.
//!
//! It is idempotent: what exists by name is left as it is (and the admin may
//! have changed it), and a site whose server has no A/B tables (the module is
//! not installed) gets nothing.

use sc_catalog::Catalog;
use sc_dataset::{DatasetDef, Op};
use sc_error::Result;
use serde_json::{Value as Json, json};
use uuid::Uuid;

use crate::panel::{Panel, PanelBody, check_state};
use crate::plot::{AggregateFn, Cell, DataRef, FieldDef, TableSpec};
use crate::workspace::{
    Workspace, WorkspaceKind, bootstrap_workspaces, create_workspace, list_workspaces,
};

/// The exposures table the Astro module declares.
pub const EXPOSURES_TABLE: &str = "astro_exposures";
/// The conversions table the Astro module declares.
pub const CONVERSIONS_TABLE: &str = "astro_conversions";

/// The workspace's name for `site`.
pub fn workspace_name(site: &str) -> String {
    format!("A/B tests: {}", site.trim())
}

/// The datasets' names for `site`: exposures, then conversions.
pub fn dataset_names(site: &str) -> (String, String) {
    (
        format!("A/B exposures: {}", site.trim()),
        format!("A/B conversions: {}", site.trim()),
    )
}

/// What the dashboard says at its top.
const EXPLANATION: &str = "Each **experiment** is an `<Optimise>` on the site, and each \
**variant** one of its `<Variant>`s. *Visitors* is how many were shown the variant, and the mean \
of *converted* is the share of them who then reached a goal (a `<Goal>` link, or `goal()` \
after a form). Compare variants within an experiment; a difference over a few dozen visitors \
is usually chance. Conversions by goal counts each goal once per visitor and experiment.";

/// Make the A/B tests workspace for `site` (an application's name) if it is
/// not there, with the datasets it reads. Answers the workspace — the one
/// that was there, or the new one — or `None` when the server has no A/B
/// tables.
pub async fn ensure_ab_workspace(catalog: &Catalog, site: &str) -> Result<Option<Workspace>> {
    if catalog.get(EXPOSURES_TABLE)?.is_none() || catalog.get(CONVERSIONS_TABLE)?.is_none() {
        return Ok(None);
    }
    bootstrap_workspaces(catalog).await?;
    let name = workspace_name(site);
    if let Some(existing) = list_workspaces(catalog)
        .await?
        .into_iter()
        .find(|w| w.name == name)
    {
        return Ok(Some(existing));
    }
    let (exposures_name, conversions_name) = dataset_names(site);
    let exposures = ensure_dataset(
        catalog,
        &exposures_name,
        EXPOSURES_TABLE,
        site,
        "Which variant of each experiment the site's visitors were shown, and whether they converted.",
    )
    .await?;
    let conversions = ensure_dataset(
        catalog,
        &conversions_name,
        CONVERSIONS_TABLE,
        site,
        "The goals the site's visitors reached, once per visitor, experiment and goal.",
    )
    .await?;
    let mut workspace = Workspace::new(name, WorkspaceKind::Dashboard, None);
    workspace.state = dashboard_state(exposures, conversions)?;
    check_state(workspace.kind, &workspace.state)?;
    create_workspace(catalog, &workspace).await?;
    Ok(Some(workspace))
}

/// The dataset `name` over `table`'s rows for `site`, made unless it is there.
async fn ensure_dataset(
    catalog: &Catalog,
    name: &str,
    table: &str,
    site: &str,
    description: &str,
) -> Result<sc_dataset::DatasetId> {
    sc_dataset::bootstrap_datasets(catalog).await?;
    if let Some(def) = sc_dataset::load_dataset_by_name(catalog, name).await? {
        return Ok(def.id);
    }
    // A JSON string is a formula's string literal, quotes and all.
    let literal = Json::String(site.trim().to_owned()).to_string();
    let mut def =
        DatasetDef::over_table(name, table).then(Op::filter(format!("site === {literal}")));
    def.description = description.to_owned();
    sc_dataset::save_dataset(catalog, &def).await?;
    Ok(def.id)
}

/// The dashboard: the explanation across the top, the two tables below it.
fn dashboard_state(
    exposures: sc_dataset::DatasetId,
    conversions: sc_dataset::DatasetId,
) -> Result<Json> {
    let by_variant = vec![FieldDef::of("experiment"), FieldDef::of("variant")];
    let text = titled(
        "How to read this",
        PanelBody::Text {
            markdown: EXPLANATION.to_owned(),
        },
    );
    let rates = titled(
        "Visitors and conversion rate by variant",
        PanelBody::SummaryTable {
            spec: TableSpec {
                data: DataRef::Dataset { dataset: exposures },
                fold: None,
                rows: by_variant.clone(),
                columns: Vec::new(),
                cells: vec![Cell::count(), Cell::of(AggregateFn::Mean, "converted")],
                totals: false,
            },
        },
    );
    let goals = titled(
        "Conversions by goal",
        PanelBody::SummaryTable {
            spec: TableSpec {
                data: DataRef::Dataset {
                    dataset: conversions,
                },
                fold: None,
                rows: by_variant,
                columns: vec![FieldDef::of("goal")],
                cells: vec![Cell::count()],
                totals: true,
            },
        },
    );
    let tile = |panel: Panel, x: u64, y: u64, w: u64, h: u64| -> Result<Json> {
        Ok(json!({
            "id": Uuid::new_v4().to_string(),
            "panel": serde_json::to_value(panel)
                .map_err(|e| sc_error::Error::invalid(format!("a panel did not serialise: {e}")))?,
            "x": x, "y": y, "w": w, "h": h,
        }))
    };
    Ok(json!({
        "tiles": [
            tile(text, 0, 0, 12, 3)?,
            tile(rates, 0, 3, 6, 10)?,
            tile(goals, 6, 3, 6, 10)?,
        ],
    }))
}

fn titled(title: &str, body: PanelBody) -> Panel {
    Panel {
        title: Some(title.to_owned()),
        ..Panel::new(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dashboard_is_one_the_analytics_ui_accepts() {
        let state =
            dashboard_state(sc_dataset::DatasetId::new(), sc_dataset::DatasetId::new()).unwrap();
        check_state(WorkspaceKind::Dashboard, &state).unwrap();
        let tiles = state["tiles"].as_array().unwrap();
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[1]["panel"]["kind"], "summary_table");
        assert_eq!(
            tiles[1]["panel"]["content"]["spec"]["rows"][1]["field"],
            "variant"
        );
    }
}
