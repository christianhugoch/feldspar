// `#scbuildform`, which v1's builder submits and this server does not post to
// (TODO "The builder" §4, 8.4).
//
// v1's *Next* (`Builder.js`'s `NextButton`) writes the layout and the columns
// into the form's hidden inputs as encoded JSON and calls `form.submit()`. In v1
// the form posts to the workflow route, which puts them into the configuration
// and runs the next step. There is no such route here, so the host takes the
// two inputs to the typed client:
//
// - a view: `saveViewLayout` at the step this document builds;
// - a page: `savePageLayout`.
//
// On success it goes to `afterSave`, which the builder route computed: the
// wizard's next step, the view list after a view's last step, or the Pages tab.
//
// - `form.submit()` fires no `submit` event, so the form's own `submit` is
//   replaced. A `submit` event (Enter in a field) is the same save.
// - A refused save is shown with v1's `notifyAlert`, and the canvas stays as it
//   is, so nothing the admin built is lost.
// - It waits for the builder's other writes before leaving. *Next* sends a
//   shared component's edits as a separate request just before it submits
//   (with `keepalive` in v1), and navigating away would cancel it.
//
// The autosave does not come through here: `Library.js` posts to `savebuilder`,
// which `builder-fetch.ts` maps to the same two calls, and v1 shows its refusals.

import type { ApiClient } from "./client";
import { writesInFlight } from "./builder-fetch";
import type { BuilderTarget } from "./context";
import { notify } from "./notify";

export interface SaveFormTarget {
  /** The application's id, as the admin API names it. */
  application: string;
  target: BuilderTarget;
  /** Where the admin goes once the layout is saved. */
  afterSave: string;
}

/** One of the form's hidden inputs, as `NextButton` wrote it: encoded JSON, or
 * nothing. */
function readInput(form: HTMLFormElement, name: string): unknown {
  const input = form.querySelector<HTMLInputElement>(`input[name="${name}"]`);
  // `NextButton` sets the attribute, not the property.
  const raw = input?.getAttribute("value") || input?.value || "";
  return raw ? JSON.parse(decodeURIComponent(raw)) : null;
}

/** Take over `form`'s submit. Answers the save itself, which resolves to
 * whether it saved. */
export function installSaveForm(
  form: HTMLFormElement,
  save: SaveFormTarget,
  client: ApiClient,
  navigate: (url: string) => void = (url) => window.location.assign(url),
): () => Promise<boolean> {
  let saving = false;
  const run = async (): Promise<boolean> => {
    if (saving) return false;
    saving = true;
    try {
      const layout = readInput(form, "layout");
      if (save.target.kind === "view") {
        await client.saveViewLayout(save.application, save.target.name, {
          step: save.target.step,
          columns: readInput(form, "columns"),
          layout,
        });
      } else {
        await client.savePageLayout(save.application, save.target.name, { layout });
      }
      await writesInFlight();
      navigate(save.afterSave);
      return true;
    } catch (e) {
      notify({ type: "danger", text: e instanceof Error ? e.message : String(e) });
      return false;
    } finally {
      saving = false;
    }
  };
  form.submit = () => {
    void run();
  };
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    void run();
  });
  return run;
}
