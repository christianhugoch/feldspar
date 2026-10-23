// The Astro framework end to end, without a Saltcorn server: scaffold a
// project with the module's own generators, install its two dependencies,
// build it, and run the code body the build wrote the way the server does —
// as the body of an async function whose parameters are a code run's names —
// with a stand-in `db`.
//
//     node --test plugins/astro/test/
//
// Needs npm and the registry (or a warm npm cache). Set
// SC_ASTRO_NODE_MODULES to an existing node_modules with @astrojs/compiler and
// esbuild in it to skip the install.

import { test, before } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const astro = require("../index.js");
const framework = astro.frameworks.astro;

const project = fs.mkdtempSync(path.join(os.tmpdir(), "feldspar-astro-"));

const ctx = {
  project: "site",
  name: "site",
  runtime: "src/feldspar",
  client: "src/feldspar/client.ts",
  auth: false,
  app: { name: "Acme <Rockets>", subdomain: "acme", description: "Rockets, fast.", url: "" },
  settings: { store: "apps", project: "site", site: "https://www.acme.test" },
  tables: [
    {
      name: "posts",
      pascal: "Posts",
      title: "Posts",
      client: "posts",
      pk: "id",
      fields: [
        { name: "id", primary_key: true },
        { name: "title", primary_key: false },
      ],
    },
  ],
};

/** Write `files` (the generator's answer) under the project. */
function write(files) {
  for (const f of files) {
    const target = path.join(project, f.path);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, f.contents);
  }
}

/** A `db` that answers rows of `tables` and records what was asked. */
function fakeDb(tables) {
  const asked = [];
  const table = (name, conds = []) => {
    const matching = () =>
      (tables[name] || []).filter((row) => conds.every((c) => Object.entries(c).every(([k, v]) => row[k] === v)));
    const chain = {
      where: (c) => table(name, [...conds, c]),
      orderBy: () => chain,
      limit: () => chain,
      asUser: () => chain,
      rows: async () => {
        asked.push(name);
        return matching();
      },
      first: async () => matching()[0] ?? null,
    };
    return chain;
  };
  return { db: new Proxy({ table }, { get: (t, k) => (k in t ? t[k] : table(k)) }), asked };
}

let body;
const run = (request, { db, user = null, trigger = {} } = {}) => {
  const AsyncFunction = (async () => {}).constructor;
  const fn = new AsyncFunction("request", "user", "db", "trigger", body);
  return fn({ query: {}, method: "GET", origin: "http://acme.localhost", ...request }, user, db, trigger);
};

before(() => {
  write(framework.scaffold(ctx));
  // Pages the scaffold does not write: the shapes a blog needs.
  write([
    {
      path: "src/pages/blog/[slug].astro",
      contents: `---
export async function getStaticPaths() {
  const posts = await db.posts.rows();
  return posts.map((p) => ({ params: { slug: p.slug } }));
}
const post = await db.posts.where({ slug: Astro.params.slug }).first();
if (!post) return Astro.redirect("/404", 302);
---
<html><head><title>{post.title}</title></head>
<body><article><h1>{post.title}</h1><div set:html={post.body} /></article>
<p class:list={["meta", { draft: !post.published }]} style={{ fontSize: "0.9rem" }}>{post.author}</p>
<script>document.body.dataset.ready = "yes";</script>
</body></html>
<style>h1 { color: rebeccapurple; }</style>
`,
    },
    {
      path: "src/pages/rss.xml.ts",
      contents: `export async function GET() {
  const posts = await db.posts.rows();
  return new Response("<rss>" + posts.map((p) => "<item>" + p.title + "</item>").join("") + "</rss>", {
    headers: { "content-type": "application/rss+xml" },
  });
}
`,
    },
    {
      path: "src/pages/_draft.astro",
      contents: "<p>not a route</p>",
    },
  ]);
  const shared = process.env.SC_ASTRO_NODE_MODULES;
  if (shared) fs.symlinkSync(shared, path.join(project, "node_modules"), "dir");
  else execFileSync("npm", ["install", "--no-audit", "--no-fund"], { cwd: project, stdio: "inherit" });
  execFileSync("npm", ["run", "build"], { cwd: project, stdio: "inherit" });
  body = fs.readFileSync(path.join(project, "dist/_server/entry.js"), "utf8");
});

const posts = [
  { id: 1, slug: "hello", title: "Hello <world>", body: "<em>first</em>", author: "Ada", published: true },
  { id: 2, slug: "draft", title: "Draft", body: "", author: "Bo", published: false },
];

test("the build writes a server body, one stylesheet, a script file and public/", () => {
  const files = fs.readdirSync(path.join(project, "dist/_astro"));
  assert.equal(files.filter((f) => /^site\.[0-9a-f]{10}\.css$/.test(f)).length, 1, files.join());
  assert.equal(files.filter((f) => /^script\.[0-9a-f]{10}\.js$/.test(f)).length, 1, files.join());
  assert.ok(fs.existsSync(path.join(project, "dist/favicon.svg")));
  const css = fs.readFileSync(path.join(project, "dist/_astro", files.find((f) => f.endsWith(".css"))), "utf8");
  assert.match(css, /h1:where\(\.astro-[a-z0-9]+\)\{color:#639\}/, "minified and scoped");
  assert.ok(css.indexOf("box-sizing") < css.indexOf("#639"), "imported CSS comes before components'");
  assert.match(css, /:where\(\.astro-[a-z0-9]+\)/, "component styles are scoped");
  assert.ok(!/import \* as \$\$module/.test(body), "the compiler's metadata imports are gone");
});

test("a page's frontmatter reads the database with the code-action syntax", async () => {
  const { db, asked } = fakeDb({ posts });
  const out = await run({ path: "/posts" }, { db });
  assert.equal(out.status, 200);
  assert.deepEqual(asked, ["posts"]);
  assert.match(out.body, /^<!DOCTYPE html>/i);
  assert.match(out.body, /Hello &lt;world&gt;/, "text is escaped");
  assert.match(out.body, /<link rel="stylesheet" href="\/_astro\/site\.[0-9a-f]+\.css">/);
  assert.match(out.body, /<title>Posts<\/title>/);
  assert.match(out.body, /<link rel="canonical" href="https:\/\/www\.acme\.test\/posts"/);
  assert.match(out.body, /Acme &lt;Rockets&gt;/, "the app name is escaped into the layout");
});

test("dynamic routes, set:html, class:list, style objects and scripts", async () => {
  const { db } = fakeDb({ posts });
  const out = await run({ path: "/blog/hello" }, { db });
  assert.equal(out.status, 200, out.body);
  assert.match(out.body, /<em>first<\/em>/, "set:html is not escaped");
  assert.match(out.body, /class="meta astro-[a-z0-9]+"/);
  assert.match(out.body, /style="font-size:0\.9rem"/);
  assert.match(out.body, /<script type="module" src="\/_astro\/script\.[0-9a-f]+\.js"><\/script>/);
  assert.ok(!out.body.includes("dataset.ready"), "the script is a file, not inline");

  const missing = await run({ path: "/blog/nope" }, { db });
  assert.equal(missing.status, 302);
  assert.deepEqual(missing.headers.find(([k]) => k === "location"), ["location", "/404"]);
});

test("a form posts back with its CSRF field and redirects", async () => {
  const { db } = fakeDb({});
  const page = await run({ path: "/contact", csrfToken: "tok123" }, { db });
  assert.match(page.body, /<input type="hidden" name="_csrf" value="tok123">/);

  const bad = await run({ path: "/contact", method: "POST", form: [["email", "x"], ["message", ""]] }, { db });
  assert.equal(bad.status, 200);
  assert.match(bad.body, /Please give an email address/);

  const good = await run(
    { path: "/contact", method: "POST", form: [["email", "a@b.c"], ["message", "hi"]] },
    { db }
  );
  assert.equal(good.status, 303);
});

test("endpoints, the sitemap, robots.txt and the 404 page", async () => {
  const { db } = fakeDb({ posts });
  const rss = await run({ path: "/rss.xml" }, { db });
  assert.equal(rss.body, "<rss><item>Hello <world></item><item>Draft</item></rss>");
  assert.deepEqual(rss.headers.find(([k]) => k === "content-type"), ["content-type", "application/rss+xml"]);

  const map = await run({ path: "/sitemap.xml" }, { db });
  for (const loc of ["/", "/blog/hello", "/blog/draft", "/contact", "/posts"]) {
    assert.ok(map.body.includes(`<loc>https://www.acme.test${loc}</loc>`), `${loc} in ${map.body}`);
  }
  assert.ok(!map.body.includes("404"), map.body);

  const robots = await run({ path: "/robots.txt" }, { db });
  assert.match(robots.body, /Sitemap: https:\/\/www\.acme\.test\/sitemap\.xml/);

  const missing = await run({ path: "/no/such/page" }, { db });
  assert.equal(missing.status, 404);
  assert.match(missing.body, /Page not found/);

  const partial = await run({ path: "/_draft" }, { db });
  assert.equal(partial.status, 404, "a file starting with _ is not a route");
});

test("prerendering: the build is asked which pages, then renders each", async () => {
  const { db } = fakeDb({ posts });
  const list = await run({ mode: "prerender-list" }, { db });
  assert.deepEqual(list.paths.sort(), ["/", "/404"]);
  const index = await run({ mode: "prerender", path: "/" }, { db, user: { id: 7 } });
  assert.equal(index.status, 200);
  assert.match(index.body, /Rockets, fast\./);
  const notFound = await run({ mode: "prerender", path: "/404" }, { db });
  assert.equal(notFound.status, 404);
});

test("an island fails by name rather than rendering nothing", async () => {
  write([
    {
      path: "src/pages/island.astro",
      contents: `---
import Counter from "../components/Feature.astro";
---
<Counter client:load title="x" />`,
    },
  ]);
  execFileSync("npm", ["run", "build"], { cwd: project, stdio: "pipe" });
  body = fs.readFileSync(path.join(project, "dist/_server/entry.js"), "utf8");
  await assert.rejects(run({ path: "/island" }, fakeDb({})), /islands .* are not supported/);
  fs.rmSync(path.join(project, "src/pages/island.astro"));
});
