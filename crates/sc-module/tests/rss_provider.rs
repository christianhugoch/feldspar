//! The milestone's **definition of done**, against the real `@saltcorn/rss`
//! (design §8.3).
//!
//! `@saltcorn/rss` is one of the smallest v1 plugins there is and the whole of
//! it is a table provider: a `configuration_workflow` asking for a feed URL, two
//! declared columns, and a `get_table` whose `getRows` parses the feed with
//! `rss-parser`. Nothing about it was written for this version of Saltcorn, and
//! that is the point — it is loaded, configured and read here exactly as v1
//! loads, configures and reads it.
//!
//! What runs:
//!
//! - `npm install @saltcorn/rss` into a throwaway modules root, which also pulls
//!   `rss-parser` and its `xml2js`;
//! - the package loaded on a `deno_runtime` worker **in this process**, granted
//!   exactly one `host:port`;
//! - the provider's settings, its columns and its rows read through the same
//!   three calls the catalog makes;
//! - against a feed this test serves over `127.0.0.1`, so the assertion is about
//!   Saltcorn and not about somebody's website.
//!
//! `#[ignore]` on the two that reach the npm registry, which is this
//! workspace's rule for a test that needs the network:
//! `cargo test -p sc-module --features deno-host --test rss_provider -- --ignored`.

#![cfg(feature = "deno-host")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::common;

use common::{have_npm, temp_root};
use sc_module::{Installer, ModuleHost, ModulePermissions, ModuleSource};
use serde_json::json;

/// A feed with three items, two of which carry every element `rss-parser` reads.
const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Saltcorn news</title>
    <link>https://saltcorn.test/</link>
    <description>A feed for a test</description>
    <item>
      <title>Table providers land</title>
      <link>https://saltcorn.test/providers</link>
      <pubDate>Tue, 03 Jun 2025 09:00:00 GMT</pubDate>
    </item>
    <item>
      <title>Modules run in-process</title>
      <link>https://saltcorn.test/modules</link>
      <pubDate>Mon, 02 Jun 2025 09:00:00 GMT</pubDate>
    </item>
    <item>
      <title>SQLite arrives</title>
      <link>https://saltcorn.test/sqlite</link>
    </item>
  </channel>
</rss>
"#;

#[tokio::test]
#[ignore = "reaches the npm registry"]
async fn the_real_rss_module_serves_a_table_from_a_feed() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (port, server) = feed_server();
    let root = temp_root("rss-provider");
    let installer = Installer::new(&root);

    let package = installer
        .install(ModuleSource::Npm, "@saltcorn/rss")
        .await
        .unwrap();
    assert_eq!(package.name, "@saltcorn/rss");

    // One host, and it is the one the feed is on. Everything else — the
    // filesystem, the environment, every other host — stays closed, which is
    // what a module gets until an admin says otherwise.
    let permissions = ModulePermissions {
        net: vec![format!("127.0.0.1:{port}")],
        ..ModulePermissions::closed()
    };
    let host = ModuleHost::new(&root);
    let manifest = host
        .load(
            &package.name,
            &installer.package_dir(&package.name),
            &json!({}),
            &permissions,
        )
        .await
        .unwrap();

    // The module supplies one table provider and no actions — which is the
    // shape this milestone exists for, and the shape the previous one could not
    // load at all.
    assert!(manifest.actions.is_empty(), "{:?}", manifest.actions);
    assert_eq!(manifest.table_providers.len(), 1);
    let provider = &manifest.table_providers[0];
    assert_eq!(provider.name, "RSS feed");
    // Its settings, read out of its own `configuration_workflow`: one required
    // Feed URL, which is what the New table dialog renders.
    assert_eq!(provider.config_fields.len(), 1);
    assert_eq!(provider.config_fields[0]["name"], json!("url"));
    assert_eq!(provider.config_fields[0]["label"], json!("Feed URL"));
    assert_eq!(provider.config_fields[0]["required"], json!(true));
    assert!(manifest.issues.is_empty(), "{:?}", manifest.issues);

    // The columns it presents. `@saltcorn/rss` declares them as a plain array,
    // so they do not depend on the configuration.
    let feed_url = format!("http://127.0.0.1:{port}/feed.xml");
    let config = json!({ "url": feed_url });
    let fields = host
        .provider_fields(&package.name, "RSS feed", &config)
        .await
        .unwrap();
    let columns: Vec<&str> = fields
        .iter()
        .map(|f| f["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(columns, ["title", "link"], "{fields:?}");

    // And the rows — the module's own `rss-parser` opening a real socket to the
    // one host it was granted, from inside a Deno worker in this process.
    let rows = host
        .provider_rows(
            &package.name,
            "RSS feed",
            &config,
            "headlines",
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0]["title"], json!("Table providers land"));
    assert_eq!(rows[0]["link"], json!("https://saltcorn.test/providers"));
    assert_eq!(rows[2]["title"], json!("SQLite arrives"));

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
    drop(server);
}

#[tokio::test]
#[ignore = "reaches the npm registry"]
async fn the_real_rss_module_is_denied_a_host_nobody_granted_it() {
    skip_without!(have_npm(), "npm is not on the PATH");
    let (port, server) = feed_server();
    let root = temp_root("rss-denied");
    let installer = Installer::new(&root);
    let package = installer
        .install(ModuleSource::Npm, "@saltcorn/rss")
        .await
        .unwrap();

    // Closed, which is what a module is until an admin grants it something.
    let host = ModuleHost::new(&root);
    host.load(
        &package.name,
        &installer.package_dir(&package.name),
        &json!({}),
        &ModulePermissions::closed(),
    )
    .await
    .unwrap();

    let err = host
        .provider_rows(
            &package.name,
            "RSS feed",
            &json!({ "url": format!("http://127.0.0.1:{port}/feed.xml") }),
            "headlines",
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap_err()
        .to_string();
    // A denial is a sentence naming the module, what it wanted, and where to
    // allow it — not an `EACCES` and not an empty table.
    assert!(err.contains("@saltcorn/rss"), "{err}");
    assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");

    host.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
    drop(server);
}

/// Serve [`FEED`] over `127.0.0.1` on a port the OS chose, for as many requests
/// as the module makes.
///
/// Thirty lines of HTTP/1.1 rather than a crate, on the same grounds the mqtt
/// test writes its own subscriber: what is being tested is the module reaching
/// a socket it was granted, and a dependency here would be a dependency in the
/// way of reading that.
fn feed_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        // Two requests at most: one per test that reaches it, plus slack for a
        // retry. The thread ends with the listener either way.
        for _ in 0..4 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/rss+xml\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{FEED}",
                    FEED.len()
                )
                .as_bytes(),
            );
        }
    });
    (port, server)
}
