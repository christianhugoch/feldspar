// `fetch`, as the vendored builder sees it (TODO "The builder" §3).
//
// The build rewrites the free `fetch` identifier of every file in `vendor/`, and
// of no other file, to this function (`build.mjs`'s `fetchPlugin`). It matches
// the URL against `routes.ts` and answers with a `Response` shaped as v1's route
// answered it:
//
// - a **mapped** request becomes a call on the typed admin client;
// - a **refused** one answers `{ error }` with the route's sentence;
// - anything else, including a URL in no column, is refused naming the URL.
//   Nothing is passed through to the network.
//
// Code in `src/` calls `globalThis.fetch` (through the client) and is not
// rewritten.

import type {
  BuilderFieldviewConfigFormRequest,
  SaveViewLayoutRequest,
} from "./client";
import { builderContext, type BuilderContext } from "./context";
import { notify } from "./notify";
import { matchRoute, refusalSentence, type Params } from "./routes";

type Body = Record<string, unknown>;
type Handler = (ctx: BuilderContext, params: Params, body: Body) => Promise<Response>;

const json = (value: unknown, status = 200): Response =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const html = (text: string): Response =>
  new Response(text, { status: 200, headers: { "content-type": "text/html; charset=utf-8" } });

/** v1's error answer. The builder reads `error` from it wherever it reads one. */
export const refused = (sentence: string): Response => json({ error: sentence }, 400);

/** The view this document builds, or a refusal naming the mismatch. */
function viewTarget(ctx: BuilderContext): { name: string; step: number } {
  if (ctx.target.kind !== "view") {
    throw new Error(`this builder edits the page ${ctx.target.name}, not a view`);
  }
  return ctx.target;
}

function pageTarget(ctx: BuilderContext): { name: string } {
  if (ctx.target.kind !== "page") {
    throw new Error(`this builder edits the view ${ctx.target.name}, not a page`);
  }
  return ctx.target;
}

type LibraryUpdates = SaveViewLayoutRequest["libraryUpdates"];

/** The builder's `libraryUpdates`, as the admin API takes them: v1's entries
 * also carry the `node_id` of the placement they were edited in, which only
 * the builder uses. */
function libraryUpdates(value: unknown): NonNullable<LibraryUpdates> {
  if (!Array.isArray(value)) return [];
  return value.map((u: { library_id: unknown; layout: unknown }) => ({
    library_id: String(u.library_id),
    layout: u.layout,
  }));
}

/** The handler for each mapped `fetch` route, keyed by its path in `routes.ts`.
 * v1's `:id` for a view or page is ignored: one document builds one target,
 * and the context names it. */
export const FETCH_HANDLERS: Record<string, Handler> = {
  "/viewedit/savebuilder/:id": async (ctx, _params, body) => {
    const view = viewTarget(ctx);
    await ctx.client.saveViewLayout(ctx.application, view.name, {
      step: view.step,
      columns: body.columns,
      layout: body.layout,
      libraryUpdates: libraryUpdates(body.libraryUpdates),
    });
    return json({ success: "ok" });
  },
  "/pageedit/savebuilder/:id": async (ctx, _params, body) => {
    await ctx.client.savePageLayout(ctx.application, pageTarget(ctx).name, {
      layout: body.layout,
      libraryUpdates: libraryUpdates(body.libraryUpdates),
    });
    return json({ success: "ok" });
  },
  "/viewedit/getlayout/:id": async (ctx) => {
    const view = await ctx.client.getView(ctx.application, viewTarget(ctx).name);
    const configuration = (view.configuration ?? {}) as { layout?: unknown };
    return json({ layout: configuration.layout });
  },
  "/pageedit/getlayout/:id": async (ctx) => {
    const page = await ctx.client.getPage(ctx.application, pageTarget(ctx).name);
    return json({ layout: page.layout });
  },
  "/library/content/:id": async (ctx, params) =>
    json(await ctx.client.getLibraryItem(ctx.application, params.id)),
  "/library/savefrombuilder": async (ctx, _params, body) =>
    json(
      await ctx.client.createLibraryItem(ctx.application, {
        name: String(body.name ?? ""),
        icon: typeof body.icon === "string" ? body.icon : null,
        layout: body.layout,
      }),
    ),
  "/library/save-updates": async (ctx, _params, body) =>
    json(
      await ctx.client.saveLibraryUpdates(ctx.application, {
        libraryUpdates: libraryUpdates(body.libraryUpdates),
      }),
    ),
  "/field/preview/:table/:field/:fieldview": async (ctx, params, body) => {
    const preview = await ctx.client.builderFieldPreview(ctx.application, {
      table: params.table,
      field: params.field,
      fieldview: params.fieldview,
      configuration: body.configuration ?? null,
      row_id: body.row_id ?? null,
    });
    return html(preview.html);
  },
  "/field/fieldviewcfgform/:table": async (ctx, params, body) =>
    json(
      await ctx.client.builderFieldviewConfigForm(ctx.application, {
        ...body,
        table: params.table,
      } as BuilderFieldviewConfigFormRequest),
    ),
  "/view/:name/preview": async (ctx, params, body) => {
    const preview = await ctx.client.builderViewPreview(ctx.application, {
      view: params.name,
      state: body,
    });
    return html(preview.html);
  },
  "/page/:name/preview": async (ctx, params) => {
    const preview = await ctx.client.builderPagePreview(ctx.application, { page: params.name });
    return html(preview.html);
  },
  "/api/:table/distinct/:field": async (ctx, params) =>
    json(await ctx.client.builderDistinctValues(ctx.application, params.table, params.field)),
  "/crashlog/": async (_ctx, _params, body) => {
    // v1 records the crash on the server. Here it goes to the console, where
    // the stack is readable, and the admin is told something went wrong.
    console.error("The builder crashed:", body.message, body.stack);
    notify({
      type: "danger",
      text: `The builder hit an error: ${String(body.message ?? "unknown")}. Details are in the browser console.`,
    });
    return json({});
  },
};

/** The URL a `fetch` was given, as a string. */
function urlOf(input: RequestInfo | URL): string {
  if (typeof input === "string") return input;
  if (input instanceof URL) return input.href;
  return input.url;
}

export async function builderFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const asked = urlOf(input);
  const url = new URL(asked, window.location.href);
  const match = url.origin === window.location.origin ? matchRoute(url.pathname) : null;
  const handler = match?.route.column === "mapped" ? FETCH_HANDLERS[match.route.path] : undefined;
  if (!match || !handler) {
    const sentence = refusalSentence(asked, match?.route ?? null, "fetch");
    if (match?.route.column !== "refused") console.error(sentence);
    return refused(sentence);
  }
  try {
    const body = typeof init?.body === "string" && init.body ? (JSON.parse(init.body) as Body) : {};
    return await handler(builderContext(), match.params, body);
  } catch (e) {
    return refused(e instanceof Error ? e.message : String(e));
  }
}
