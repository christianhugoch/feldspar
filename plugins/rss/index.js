// **An RSS or Atom feed as a read-only table** — the first of the bundled
// modules (`plugins/README.md`).
//
// It is the smallest useful thing a table provider can be, which is why it is
// the example: a feed URL is the whole configuration, the columns are fixed,
// and `getRows` is one call into `rss-parser`. There is no `@saltcorn/*` require
// anywhere in it — v1's `Workflow` and `Form` are answered by the module host as
// stubs so that *v1's own plugins* load, and a module written for this server
// has no reason to reach for them: a `configuration_workflow` is a function
// returning `{ steps: [{ form: { fields } }] }`, and that object is all the host
// reads back.
//
// What it deliberately does not do
// --------------------------------
// **Filter.** `getRows(where, options)` ignores both. A feed is fetched whole or
// not at all, so a `where` honoured here would only mean "discard some rows
// after parsing them", and the catalog applies the query's real filter to
// whatever comes back regardless (`sc-catalog`'s `matching`). Honouring `limit`
// would be actively wrong: it is a hint pushed down *before* that filter, and
// twenty rows taken off the top of the feed are not the twenty rows the query
// asked for.
//
// **Write.** No `insertRow`, `updateRow` or `deleteRows`, which is v1's way of
// saying a provided table is read-only, and the truth here: the feed belongs to
// somebody else.

const Parser = require("rss-parser");

/** The columns every feed presents, in the order a table shows them.
 *
 * `guid` is the primary key because a provided table needs one to address a row
 * by, and it is the one field a feed is *specified* to make unique. Feeds that
 * omit it fall back to the item's link, then to its position — see `rowId`. */
const FIELDS = [
  { name: "guid", label: "ID", type: "String", primary_key: true },
  { name: "title", label: "Title", type: "String" },
  { name: "link", label: "Link", type: "String" },
  { name: "published", label: "Published", type: "Date" },
  { name: "author", label: "Author", type: "String" },
  { name: "summary", label: "Summary", type: "String" },
  { name: "content", label: "Content", type: "String" },
];

/** The provider's settings: what an admin fills in when creating the table.
 *
 * One step, because there is one decision. The two below it have defaults that
 * are right for every feed, and exist for the operator whose feed is enormous
 * or whose publisher counts requests.
 */
const configuration_workflow = () => ({
  steps: [
    {
      name: "Feed",
      form: {
        fields: [
          {
            name: "url",
            label: "Feed URL",
            type: "String",
            required: true,
            sublabel: "The address of an RSS or Atom feed, including https://.",
          },
          {
            name: "max_items",
            label: "Maximum items",
            type: "Integer",
            sublabel: "Rows to keep, newest first. Blank or 0 keeps the whole feed.",
          },
          {
            name: "cache_seconds",
            label: "Cache for (seconds)",
            type: "Integer",
            sublabel:
              "How long a fetched feed is reused before the publisher is asked again. " +
              "Default 60; 0 fetches on every query.",
          },
        ],
      },
    },
  ],
});

/** One parser for the module, and a fetched feed per URL.
 *
 * Module-scope state is the reason a module is loaded once, on one worker, and
 * every call routed to it (`sc-module`'s `table_providers`): a second copy of
 * this cache on another isolate would be a second cache, and the publisher would
 * see twice the traffic the setting promises.
 */
const parser = new Parser();
const cache = new Map();

/** The parsed feed at `url`, from the cache when it is younger than `ttl`. */
async function fetchFeed(url, ttl) {
  const now = Date.now();
  const hit = cache.get(url);
  if (hit && ttl > 0 && now - hit.at < ttl * 1000) return hit.feed;
  const feed = await parser.parseURL(url);
  cache.set(url, { at: now, feed });
  return feed;
}

/** A number a setting holds, or `fallback` when it holds nothing usable.
 *
 * Settings arrive as whatever the form produced — a number, a string, `null`,
 * `undefined` — and a `NaN` cache window would be a feed fetched on every
 * keystroke of a data grid. */
function num(value, fallback) {
  const n = Number(value);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}

/** A stable id for an item: what the feed says, else its link, else where it
 * sat. The last is a poor key and it is still better than `null`, which is not
 * a key at all. */
function rowId(item, index) {
  return String(item.guid ?? item.id ?? item.link ?? `item-${index}`);
}

/** An item's publication date as something a Date column accepts: an ISO
 * string, or `null` when the feed gave no usable date. */
function published(item) {
  const raw = item.isoDate || item.pubDate || item.published || item.updated;
  if (!raw) return null;
  const at = new Date(raw);
  return Number.isNaN(at.getTime()) ? null : at.toISOString();
}

/** One feed item as a row of the table above. */
function row(item, index) {
  return {
    guid: rowId(item, index),
    title: item.title ?? null,
    link: item.link ?? null,
    published: published(item),
    author: item.creator ?? item.author ?? null,
    summary: item.contentSnippet ?? item.summary ?? null,
    content: item["content:encoded"] ?? item.content ?? null,
  };
}

module.exports = {
  sc_plugin_api_version: 1,
  table_providers: {
    "RSS feed": {
      configuration_workflow,
      fields: FIELDS,
      get_table: (config) => ({
        getRows: async () => {
          const url = String((config && config.url) || "").trim();
          if (!url) throw new Error("this RSS table has no feed URL configured");
          const feed = await fetchFeed(url, num(config.cache_seconds, 60));
          const items = Array.isArray(feed.items) ? feed.items : [];
          const rows = items.map(row);
          const max = num(config.max_items, 0);
          return max > 0 ? rows.slice(0, max) : rows;
        },
      }),
    },
  },
};
