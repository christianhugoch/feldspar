# Websites with Astro: a landing page and a blog

The React and Vue frameworks build **apps**: a bundle that runs in the browser and asks the API
for data. A **website** — a landing page for an app that lives on another subdomain, a blog, a
news page, a contact form — wants the opposite: finished HTML from the server, so search engines
read it and phones show it at once, and as little JavaScript as possible.

The bundled **Astro** module does that. You write pages in Astro's `.astro` syntax; Saltcorn
compiles them and renders them **on the server**, and a page's frontmatter reads the database
exactly as a JavaScript code action does.

What you need: a server started with a base domain (`feldspar serve --base-domain localhost`),
a local file store (`apps`, as in the [React tutorial](tutorial-react-todo.md), Step 1), and
npm on the server.

## Step 1 — Install the module

**Settings → Modules → Astro (websites) → Install.** The module itself downloads nothing; each
site's first build installs its two build tools (`@astrojs/compiler` and `esbuild`) into its own
`node_modules`.

## Step 2 — A table for the blog

**Tables → New table name** `posts`, with:

- `slug` — text, not nullable, unique
- `title` — text, not nullable
- `body` — text (HTML)
- `published_at` — timestamp
- `published` — boolean

Add a couple of rows, one with `published` ticked.

## Step 3 — Create the site

**Applications → New application**:

| Field | Value |
|---|---|
| Name | `Acme` |
| Subdomain | `www` |
| Framework | **Astro (website)** |
| File store | `apps` |
| Project directory | `www` |
| Public site URL | `https://www.acme.example` (or blank while you try it out) |
| Tables | `posts` |

Press **Create application**: the server writes the project into the store's `www` directory.
Then press **Build** on the app's row — the first build installs the two build tools — and open
`http://www.localhost:3032`. You get:

| page | file | how it is served |
|---|---|---|
| `/` | `src/pages/index.astro` | **prerendered**: written once after the build, served as a file |
| `/posts` | `src/pages/posts/index.astro` | rendered on every request, from the table |
| `/contact` | `src/pages/contact.astro` | rendered on every request; a working form |
| anything else | `src/pages/404.astro` | the not-found page, with a 404 status |
| `/sitemap.xml`, `/robots.txt` | generated | |

The **Public site URL** is what canonical links, Open Graph tags and the sitemap use. Set it
to the address people and search engines reach the site on.

## Step 4 — Read the database in a page

Open `www/src/pages/posts/index.astro`:

```astro
---
import Layout from "../../layouts/Layout.astro";

const rows = await db.posts.limit(50).rows();
---
<Layout title="Posts" description="The latest posts.">
  <h1>Posts</h1>
  <ul class="rows">
    {rows.map((row) => <li><span>{row.title}</span></li>)}
  </ul>
</Layout>
```

The part between the `---` fences is the **frontmatter**, and it is a JavaScript code body run
on the server, with everything a code action has: `db`, `fetch`, `fs`, `trigger`, `Table`,
`user`, `lib` and `console`. So you query exactly as you would in a trigger:

```js
const posts = await db.posts
  .where({ published: true })
  .orderBy("published_at", "desc")
  .limit(10)
  .rows();
```

`Table.findOne({ name: "posts" }).getRows(...)`, Saltcorn 1's spelling, works too.

**Whose data.** Like a code action, a page reads with admin authority. That is what a public
blog wants: the visitor is anonymous and the page decides what to show (`published: true`
above). To read as the visitor instead, so the table's ownership rules apply, use
`db.posts.asUser()`; `Astro.locals.user` is the signed-in user, or `null`.

## Step 5 — One page per post

Create `www/src/pages/blog/[slug].astro`:

```astro
---
import Layout from "../../layouts/Layout.astro";

// Lists the posts for /sitemap.xml. On a page that is not prerendered, that is
// all it does: any slug still reaches the page.
export async function getStaticPaths() {
  const posts = await db.posts.where({ published: true }).rows();
  return posts.map((p) => ({ params: { slug: p.slug } }));
}

const post = await db.posts.where({ slug: Astro.params.slug, published: true }).first();
if (!post) return Astro.redirect("/404");
---
<Layout title={post.title} description={post.title}>
  <article>
    <h1>{post.title}</h1>
    <div set:html={post.body} />
  </article>
</Layout>
```

and link to it from the list: `<a href={`/blog/${row.slug}`}>{row.title}</a>`. **Build** again.

- `{post.title}` is escaped; `set:html` inserts HTML as it is, so use it only for HTML you trust.
- A new post appears the moment it is saved: the page is rendered per request.
- `/sitemap.xml` now lists every published post.

**Prerender or not?** Add `export const prerender = true` to a page and it is rendered once,
after each build, and served as a file — the fastest a page can be. For a dynamic route, it then
renders exactly the pages `getStaticPaths()` lists, and any other slug is a 404. The catch is that
it shows the data as it was at the build. So: prerender the landing page, the about page and the
legal pages; leave the blog per request (or prerender it and rebuild when you publish).

## Step 6 — The contact form

`src/pages/contact.astro` posts back to itself:

```astro
---
import Layout from "../layouts/Layout.astro";
import { Csrf } from "feldspar:components";

let error = "";
if (Astro.request.method === "POST") {
  const form = await Astro.request.formData();
  const email = String(form.get("email") || "").trim();
  const message = String(form.get("message") || "").trim();
  if (!email.includes("@") || !message) {
    error = "Please give an email address and a message.";
  } else {
    await db.messages.insert({ email, message });
    return Astro.redirect("/contact?sent=1", 303);
  }
}
---
<Layout title="Contact">
  <form method="post">
    <Csrf />
    {error && <p class="error">{error}</p>}
    <input type="email" name="email" required />
    <textarea name="message" required></textarea>
    <button>Send</button>
  </form>
</Layout>
```

Three rules make a form work:

1. **`<Csrf />`** inside the form. Every form post is checked against the visitor's CSRF cookie,
   and this is the field it is checked against.
2. **No `prerender`** on a page with a form: the token is per visitor.
3. **Redirect after the write** (`303`), so a reload does not post twice.

To send an email instead of (or as well as) storing the message, make an insert trigger on
`messages` with the `send_email` action, or call a trigger directly:
`await trigger.notify_team({ email, message })`.

## Step 7 — Components, layouts, styles and scripts

- **Components** are `.astro` files with props (`Astro.props`) and slots (`<slot />`,
  `<slot name="footer" />`); see `src/components/Feature.astro`.
- **`<style>`** in a component is scoped to that component. `<style is:global>` is not.
  `import "../styles/global.css"` in a frontmatter adds a site-wide stylesheet. All of it is
  minified into one file under `/_astro/`, which browsers cache for good.
- **`<script>`** is bundled (TypeScript and npm imports work) into a file under `/_astro/`.
  `<script is:inline>` is **blocked** by the site's Content-Security-Policy unless you loosen
  `script-src` on the application.
- **Images**: put them in `public/` and refer to them as `/hero.png`, or import one in a
  frontmatter (`import hero from "../assets/hero.png"`) and use `hero.src` — it is copied under
  `/_astro/` with a hash in its name. Images are not resized.
- **Endpoints**: `src/pages/rss.xml.ts` exporting `async function GET()` that returns a
  `Response` answers `/rss.xml`.
- A file or directory whose name starts with `_` in `src/pages` is not a route.

## What is different from Astro

Saltcorn uses Astro's **compiler**, not Astro. So:

| Astro | here |
|---|---|
| `astro.config.mjs`, `astro dev`, integrations | none; `npm run build` (the Build button) builds, the server serves |
| React/Vue/Svelte components, `client:*` islands | not supported — an error naming the component. Write a `<script>` |
| Markdown/MDX pages, content collections | not supported; keep content in a table |
| `output: "static"` (prerender by default) | the opposite: **per request** by default, `prerender = true` to opt in |
| `Astro.url` is a `URL` | an object with `origin`, `pathname`, `search`, `searchParams`, `href` — the server has no `URL` class |
| `Astro.cookies.get` reads the request's cookies | sees only cookies the page itself set |
| `<Image />`, `astro:assets` | not yet |

## Speed and search engines

- Prerendered pages and everything in `public/` and `/_astro/` are served as files, with ETags;
  hashed files are cached by browsers indefinitely.
- Per-request pages are sent `Cache-Control: private, no-cache` unless the page says otherwise,
  because they may carry the visitor's CSRF token. A page that is the same for everyone can
  say so: `Astro.response.headers.set("cache-control", "public, max-age=60")`.
- The scaffolded layout writes the title, description, canonical link and Open Graph tags from
  each page's props. Give every page a `title` and a `description`.
- Each page runs within a code action's limits (5 s, and its call budget). A page making dozens
  of queries is a page to rewrite with one `.select(...)` and a Ⱶ-join.

## Next: A/B testing

Optimising a landing page's copy is designed but not built: `<Optimise>` and `<Variant>`
components that pick a variant on the server, so there is no flicker and no testing script, and
a `<Goal>` that records conversions. See the design document, §13.3, "Designed for later: A/B
optimisation of landing pages".
