// The language picker in the sidebar's account row (design §16.1, task 3.3).
//
// It writes the signed-in admin's **`language` column**, not a cookie and not
// `localStorage`. The column is the second source a request's locale is
// negotiated from, above the cookie and above `Accept-Language`, so a choice
// made here follows the person to their other browser — which is what somebody
// who has just told a machine what language they read expects.
//
// Saving reloads the page. That is not laziness: the locale is negotiated
// **once per request** and everything the server said on this page — every
// label out of a `config_spec`, every settings heading, every refusal — was
// said in the old one. Re-rendering the SPA's own literals while the server's
// half stayed French would be the half-translated page the whole design is
// arranged to avoid.
//
// **Nothing renders at all on a monolingual installation** (D11).

import { useState } from "react";

import { api } from "../api";
import type { AuthStatusResponse } from "../client";
import type { CurrentUser } from "../App";
import { useT } from "../i18n";
import { isMultilingual, localeLabel } from "../locales";

export function LocalePicker({
  user,
  locales,
  folded,
}: {
  user: CurrentUser;
  locales: AuthStatusResponse["locales"];
  /** The sidebar rail: a select with no room for a label. */
  folded?: boolean;
}): JSX.Element | null {
  const { t, locale } = useT();
  const [saving, setSaving] = useState(false);

  if (!isMultilingual(locales)) return null;

  const choose = async (tag: string) => {
    setSaving(true);
    try {
      await api.updateUser(user.id, {
        email: user.email,
        role: user.role,
        language: tag,
      });
      // The whole page, because the server said half of it.
      window.location.reload();
    } catch {
      // A language that could not be saved is not worth an alert over the
      // layout: the select snaps back to what is stored, which is the truth.
      setSaving(false);
    }
  };

  return (
    <select
      className="form-select form-select-sm w-auto"
      value={locale}
      disabled={saving}
      aria-label={t("Language")}
      title={t("Language")}
      onChange={(e) => void choose(e.target.value)}
    >
      {locales.enabled.map((tag) => (
        <option key={tag} value={tag}>
          {folded ? tag : localeLabel(tag)}
        </option>
      ))}
    </select>
  );
}
