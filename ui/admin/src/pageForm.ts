// A Saltcorn UI page's properties form (TODO "The builder" §7, 9.3).
//
// v1's `pagePropertiesForm`, less `html_file` (out of scope): name, title,
// description, minimum role, `no_menu` and `request_fluid_layout`. The last two
// live in the page's `attributes`, beside `root_page_for_roles`, which this form
// leaves alone. It saves through `savePage`, which creates a page under a name
// it does not have and **replaces** one it does — which is why a new page's name
// must not be one the application already has: the server would take it as an
// edit of that page and replace its layout with an empty one.

import type { SavePageRequest } from "./client";
import type { Roles } from "./roles";
import type { PageItem } from "./views";

/** What the page properties form holds. */
export type PageForm = {
  name: string;
  title: string;
  description: string;
  min_role: number;
  no_menu: boolean;
  request_fluid_layout: boolean;
};

/** A new page's form: public, with the menu, in a fixed-width container. */
export function newPageForm(): PageForm {
  return {
    name: "",
    title: "",
    description: "",
    min_role: 100,
    no_menu: false,
    request_fluid_layout: false,
  };
}

/** The form over an existing page. */
export function pageFormOf(page: PageItem): PageForm {
  const attributes = attributesOf(page);
  return {
    name: page.name,
    title: page.title,
    description: page.description,
    min_role: page.min_role,
    no_menu: attributes.no_menu === true,
    request_fluid_layout: attributes.request_fluid_layout === true,
  };
}

/** Why the form cannot be saved yet, per field, or an empty object.
 *
 * `original` is the name of the page being edited, `null` for a new one. The
 * server checks the name's characters itself; this stops the incomplete and
 * the two mistakes it would not catch as such: a name another page has (which
 * `savePage` would take as that page), and a role nobody has. */
export function pageFormErrors(
  form: PageForm,
  pages: Pick<PageItem, "name">[],
  original: string | null,
  roles: Roles,
): { name?: string; min_role?: string } {
  const errors: { name?: string; min_role?: string } = {};
  const name = form.name.trim();
  if (!name) {
    errors.name = "A page needs a name.";
  } else if (name !== original && pages.some((p) => p.name === name)) {
    errors.name = `This application already has a page named "${name}".`;
  }
  // While the roles are loading there is nothing to check against.
  if (roles !== null && !roles.some((r) => r.role === form.min_role)) {
    errors.min_role = `There is no role ${form.min_role}: choose one of the roles.`;
  }
  return errors;
}

/** The `savePage` body of the form: `page`'s layout and other attributes kept,
 * and a new page's layout `{}`, as in v1. A flag that is off is removed from
 * `attributes` rather than stored as `false`, which is how the v1 import writes
 * them. */
export function savePageBody(form: PageForm, page: PageItem | null): SavePageRequest {
  const attributes: Record<string, unknown> = { ...(page ? attributesOf(page) : {}) };
  for (const flag of ["no_menu", "request_fluid_layout"] as const) {
    if (form[flag]) attributes[flag] = true;
    else delete attributes[flag];
  }
  return {
    name: form.name.trim(),
    title: form.title.trim(),
    description: form.description.trim(),
    layout: page ? page.layout : {},
    min_role: form.min_role,
    attributes,
  };
}

function attributesOf(page: PageItem): Record<string, unknown> {
  const attributes = page.attributes;
  return attributes && typeof attributes === "object" && !Array.isArray(attributes)
    ? (attributes as Record<string, unknown>)
    : {};
}
