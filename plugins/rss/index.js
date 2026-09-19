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

/** The settings a **stream** over a feed takes (TODO "Streams" §12).
 *
 * Nearly the table provider's, and deliberately not shared with it: a table is
 * queried when somebody looks at it, so its setting is a *cache window*, while a
 * stream is polled on a clock, so its setting is an *interval*. Writing one
 * field that meant both would be a setting whose sublabel had to explain which
 * of the two it was doing today.
 */
const STREAM_CONFIG_FIELDS = [
  {
    name: "url",
    label: "Feed URL",
    type: "String",
    required: true,
    sublabel: "The address of an RSS or Atom feed, including https://.",
  },
  {
    name: "interval_s",
    label: "Poll every (seconds)",
    type: "Integer",
    default: 300,
    sublabel:
      "How often the publisher is asked for new items. Five minutes by default: a feed is " +
      "not a broker, and polling one every second is somebody else's bandwidth.",
  },
  {
    name: "max_items",
    label: "Maximum items per poll",
    type: "Integer",
    sublabel: "Elements to deliver from one poll, newest first. Blank or 0 delivers all of them.",
  },
];

/** What an element of a feed stream *is*: one item, with the columns the table
 * provider declares minus the ones a stream has no use for.
 *
 * A function of the configuration, which is the shape the host asks for even
 * though this feed's answer does not depend on it — every item of every feed has
 * the same seven fields, and inventing a setting to vary them would be inventing
 * a setting.
 *
 * `guid` is `required`, so an item with no id at all is counted as malformed
 * rather than delivered: it is the one field a consumer needs to tell two
 * elements apart, and `rowId` below gives every item one.
 */
const STREAM_ELEMENT_TYPE = {
  kind: "json",
  keys: [
    { name: "guid", type: "text", required: true },
    { name: "title", type: "text" },
    { name: "link", type: "text" },
    { name: "published", type: "text" },
    { name: "author", type: "text" },
    { name: "summary", type: "text" },
    { name: "content", type: "text" },
  ],
};

/** How many item ids a cursor remembers.
 *
 * The cursor is "what I delivered last time", and it has to be a *set* rather
 * than a high-water mark because a feed is not ordered by anything this module
 * can trust: publishers reorder, backdate and edit. Capped because it is carried
 * across the module seam on every poll, and an unbounded one would grow for as
 * long as the stream runs.
 */
const CURSOR_ITEMS = 500;

module.exports = {
  sc_plugin_api_version: 1,
  /** **A feed as a dataflow** (TODO "Streams" §12): the same feed the table
   * provider above reads, delivered item by item as it changes.
   *
   * Poll, not push, because that is what a module can be: a module call is
   * request/response on a worker and there is no channel from one back into the
   * host, so what is declared here is a `poll` and the host supplies the
   * interval loop, the cursor and the decoding.
   *
   * **The first poll delivers the feed as it stands.** Backfill is out of scope
   * for streams and this is not it: a feed is a window on the last *n* items and
   * not a log, so "everything in the window when I connected" is the closest a
   * feed has to MQTT's retained message — and a stream that showed nothing at
   * all until the publisher next posted would look broken for hours.
   */
  streamproviders: {
    rss_feed: {
      label: "RSS feed",
      description: "An RSS or Atom feed, polled: one element per new item.",
      config_fields: STREAM_CONFIG_FIELDS,
      element_type: () => STREAM_ELEMENT_TYPE,
      poll: async ({ configuration, cursor }) => {
        const config = configuration || {};
        const url = String(config.url || "").trim();
        if (!url) throw new Error("this RSS stream has no feed URL configured");
        // No cache window: the poll interval *is* the cache, and reusing a
        // fetch here would mean a stream polled every 30 seconds quietly
        // delivering nothing for the other 30.
        const feed = await fetchFeed(url, 0);
        const items = Array.isArray(feed.items) ? feed.items : [];
        const max = num(config.max_items, 0);
        const all = items.map(row);
        const kept = max > 0 ? all.slice(0, max) : all;
        // The first poll has no cursor and delivers the window; every one after
        // it delivers what was not in the last.
        const seen = Array.isArray(cursor) ? new Set(cursor) : null;
        const fresh = seen ? kept.filter((item) => !seen.has(item.guid)) : kept;
        return {
          elements: fresh,
          // What this poll *saw*, not what it delivered: an item that was in
          // the window last time and is still there must not be delivered
          // again because it happened to fall past `max_items`.
          cursor: all.slice(0, CURSOR_ITEMS).map((item) => item.guid),
        };
      },
    },
  },
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
