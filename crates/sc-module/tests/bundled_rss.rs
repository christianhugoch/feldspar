//! `plugins/rss` — the bundled RSS module — installed from the directory it
//! ships in and read as a table.
//!
//! `rss_provider.rs` does this against v1's `@saltcorn/rss`, which is the
//! compatibility claim: somebody else's plugin, loaded and read unchanged.
//! This is the other one. `plugins/rss` is *ours*: it ships inside the release
//! tarball, it is installed from a directory rather than from a registry, and
//! it is written against this server's own reading of a table provider rather
//! than against v1's `Workflow` and `Form`. What is asserted is that all of
//! that is a working module — the settings the New table dialog renders, the
//! columns the table gets, and the rows, off a feed this test serves on
//! `127.0.0.1`.
//!
//! `#[ignore]`: installing it runs `npm install`, which downloads `rss-parser`.
//! That is the design rather than an inconvenience — the dependency is
//! deliberately not in the tarball — and it is this workspace's rule that a test
//! reaching the network is opt-in:
//! `cargo test -p sc-module --features deno-host --test it bundled_rss -- --ignored`.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use common::{have_npm, temp_root};
use sc_module::{ANY_HOST, BundledModules, Installer, ModuleHost, ModulePermissions, ModuleSource};
use serde_json::json;

/// A feed exercising every column the module declares: an item with all of
/// them, one with no `guid` (so the link becomes the key), and one with neither
/// a guid nor a date.
const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Feldspar news</title>
    <link>https://feldspar.test/</link>
    <description>A feed for a test</description>
    <item>
      <guid>urn:feldspar:1</guid>
      <title>Bundled modules land</title>
      <link>https://feldspar.test/bundled</link>
      <pubDate>Tue, 03 Jun 2025 09:00:00 GMT</pubDate>
      <dc:creator>A Writer</dc:creator>
      <description>The short version.</description>
      <content:encoded><![CDATA[<p>The long version.</p>]]></content:encoded>
    </item>
    <item>
      <title>No guid here</title>
      <link>https://feldspar.test/no-guid</link>
      <pubDate>Mon, 02 Jun 2025 09:00:00 GMT</pubDate>
    </item>
    <item>
      <title>Nothing but a title</title>
    </item>
  </channel>
</rss>
"#;

/// The bundled RSS module, installed into a throwaway root from the directory it
/// ships in — which is exactly what the Modules tab's Install button does, minus
/// the row and the HTTP.
async fn install_bundled_rss(tag: &str) -> (std::path::PathBuf, Installer, String) {
    let catalog = BundledModules::discover(None);
    let entry = catalog.get("rss").expect("the RSS module ships");
    let root = temp_root(tag);
    let installer = Installer::new(&root);
    let package = installer
        .install(ModuleSource::Local, &entry.directory.display().to_string())
        .await
        .unwrap_or_else(|e| panic!("installing plugins/rss: {e}"));
    assert_eq!(package.name, entry.name);
    (root, installer, package.name)
}

#[tokio::test]
#[ignore = "installs the bundled module, which downloads rss-parser from npm"]
async fn the_bundled_rss_module_serves_a_table_from_a_feed() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (port, server) = common::http_server("application/rss+xml", FEED);
    let (root, installer, name) = install_bundled_rss("bundled-rss").await;

    // The grant its catalog card asks for and the Install button gives it: any
    // host, because a feed's address is typed into the *table's* settings and
    // cannot be on a list drawn up when the module was installed.
    let permissions = ModulePermissions {
        net: vec![ANY_HOST.to_owned()],
        ..ModulePermissions::closed()
    };
    let host = ModuleHost::new(&root);
    let manifest = host
        .load(
            &name,
            &installer.package_dir(&name),
            &json!({}),
            &permissions,
        )
        .await
        .unwrap();

    // One table provider, no actions, and nothing this version cannot read.
    assert!(manifest.actions.is_empty(), "{:?}", manifest.actions);
    assert!(manifest.issues.is_empty(), "{:?}", manifest.issues);
    assert_eq!(manifest.table_providers.len(), 1);
    let provider = &manifest.table_providers[0];
    assert_eq!(provider.name, "RSS feed");

    // Its settings, as the New table dialog renders them: the feed URL is the
    // one an admin must fill in, and the other two have defaults.
    let names: Vec<&str> = provider
        .config_fields
        .iter()
        .map(|f| f["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, ["url", "max_items", "cache_seconds"]);
    assert_eq!(provider.config_fields[0]["label"], json!("Feed URL"));
    assert_eq!(provider.config_fields[0]["required"], json!(true));

    // The columns.
    let config = json!({
        "url": format!("http://127.0.0.1:{port}/feed.xml"),
        // Off, so the third read below fetches again rather than answering from
        // the cache — the cache has its own assertion.
        "cache_seconds": 0,
    });
    let fields = host
        .provider_fields(&name, "RSS feed", &config)
        .await
        .unwrap();
    let columns: Vec<&str> = fields
        .iter()
        .map(|f| f["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        columns,
        [
            "guid",
            "title",
            "link",
            "published",
            "author",
            "summary",
            "content"
        ]
    );
    // A provided table needs a key to address a row by, and `guid` is it.
    assert_eq!(fields[0]["primary_key"], json!(true));

    // And the rows — the module's own `rss-parser` opening a socket from inside
    // a Deno worker in this process.
    let rows = host
        .provider_rows(
            &name,
            "RSS feed",
            &config,
            "headlines",
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");

    assert_eq!(rows[0]["guid"], json!("urn:feldspar:1"));
    assert_eq!(rows[0]["title"], json!("Bundled modules land"));
    assert_eq!(rows[0]["link"], json!("https://feldspar.test/bundled"));
    assert_eq!(rows[0]["author"], json!("A Writer"));
    assert_eq!(rows[0]["summary"], json!("The short version."));
    assert_eq!(rows[0]["content"], json!("<p>The long version.</p>"));
    // A date column gets something a date column accepts, whatever spelling the
    // feed used.
    assert_eq!(rows[0]["published"], json!("2025-06-03T09:00:00.000Z"));

    // No guid: the link is the key, because a row with a null key is a row
    // nothing can address.
    assert_eq!(rows[1]["guid"], json!("https://feldspar.test/no-guid"));
    // Neither: its position, which is a poor key and better than none.
    assert_eq!(rows[2]["guid"], json!("item-2"));
    // Nothing to say is null rather than an empty string, so a view can tell
    // "the feed omitted this" from "the feed said nothing".
    assert_eq!(rows[2]["link"], json!(null));
    assert_eq!(rows[2]["published"], json!(null));

    // `max_items` keeps the top of the feed and nothing else.
    let capped = json!({
        "url": format!("http://127.0.0.1:{port}/feed.xml"),
        "max_items": 2,
        "cache_seconds": 0,
    });
    let rows = host
        .provider_rows(
            &name,
            "RSS feed",
            &capped,
            "headlines",
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");

    // It is read-only: no `insertRow`, `updateRow` or `deleteRows`, which is how
    // a v1 provider says a table cannot be written — the feed is somebody
    // else's.
    let writes = host
        .provider_writes(&name, "RSS feed", &config, "headlines")
        .await
        .unwrap();
    assert_eq!(writes["insert"], json!(false), "{writes}");
    assert_eq!(writes["update"], json!(false), "{writes}");
    assert_eq!(writes["delete"], json!(false), "{writes}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
    drop(server);
}

#[tokio::test]
#[ignore = "installs the bundled module, which downloads rss-parser from npm"]
async fn the_bundled_rss_module_is_denied_a_host_nobody_granted_it() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (port, server) = common::http_server("application/rss+xml", FEED);
    let (root, installer, name) = install_bundled_rss("bundled-rss-denied").await;

    // Installed without the grant its card asks for — which is what an admin
    // who narrowed its permissions afterwards has. The module is ours and it is
    // sandboxed like any other.
    let host = ModuleHost::new(&root);
    host.load(
        &name,
        &installer.package_dir(&name),
        &json!({}),
        &ModulePermissions::closed(),
    )
    .await
    .unwrap();

    let err = host
        .provider_rows(
            &name,
            "RSS feed",
            &json!({ "url": format!("http://127.0.0.1:{port}/feed.xml"), "cache_seconds": 0 }),
            "headlines",
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(&name), "{err}");
    assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
    drop(server);
}

#[tokio::test]
#[ignore = "installs the bundled module, which downloads rss-parser from npm"]
async fn a_table_with_no_feed_url_says_so_rather_than_serving_nothing() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (root, installer, name) = install_bundled_rss("bundled-rss-unconfigured").await;
    let host = ModuleHost::new(&root);
    host.load(
        &name,
        &installer.package_dir(&name),
        &json!({}),
        &ModulePermissions {
            net: vec![ANY_HOST.to_owned()],
            ..ModulePermissions::closed()
        },
    )
    .await
    .unwrap();

    // An empty table would look like a feed with no items, which is the one
    // wrong answer available here.
    let err = host
        .provider_rows(
            &name,
            "RSS feed",
            &json!({}),
            "headlines",
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("feed URL"), "{err}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
