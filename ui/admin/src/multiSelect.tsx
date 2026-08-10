// A multi-select over a list the *server* supplies: a box showing what is
// chosen, and a menu of tick boxes with "All", "None" and a filter.
//
// It exists because the application form asked an admin to type a comma-
// separated list of table and file-store names. A name typed there is only
// checked when the application is saved (and a name that no longer exists is not
// checked at all until the app fails to mount), while the correct answers are a
// list the server can hand over — the same reasoning that turned the Key field's
// three text boxes into selects.
//
// It is hand-built rather than `react-select` because the admin SPA is served
// under a strict Content-Security-Policy with `style-src 'self'` and no
// `unsafe-inline` (`sc-server`'s `CONTENT_SECURITY_POLICY`, asserted by
// `admin_theme.rs`): `react-select` styles itself with Emotion, which injects a
// stylesheet at runtime and would be blocked. For the same reason there is no
// Popper-positioned dropdown and no `style` attribute anywhere here — the menu
// is placed by rules in `admin.css`.
//
// The parts worth asserting without a browser — which entries a list of options
// and a stored selection produce, what the filter matches, and what a tick
// changes — are the exported functions below, covered in `multiSelect.test.ts`.

import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";

/** One thing that can be chosen. `label` defaults to the value, which is what a
 * table or file store wants: the name *is* the label, and the description is the
 * extra line under it. */
export type MultiChoice = {
  value: string;
  label?: string;
  description?: string;
};

/** A choice as the menu renders it: always a label, and a flag for a stored
 * value the server did not offer. */
export type MultiEntry = {
  value: string;
  label: string;
  description: string;
  /** The value is selected but is not among the server's options — a table that
   * was dropped, a file store that was deleted. */
  missing: boolean;
};

/**
 * The menu's rows: the server's options in the order it listed them, then any
 * selected value the server did not offer, flagged.
 *
 * The flagged tail is the point. A stored name that no longer resolves is an
 * application that will not mount, and a picker that only knows about things
 * that exist would either drop the name silently on the next save or hide the
 * reason the app is broken. It stays visible, and stays selected, until the
 * admin unticks it.
 */
export function mergeChoices(options: MultiChoice[], selected: string[]): MultiEntry[] {
  const entries: MultiEntry[] = options.map((o) => ({
    value: o.value,
    label: o.label?.trim() ? o.label : o.value,
    description: o.description ?? "",
    missing: false,
  }));
  for (const value of selected) {
    if (options.some((o) => o.value === value)) continue;
    if (entries.some((e) => e.value === value)) continue;
    entries.push({ value, label: value, description: "", missing: true });
  }
  return entries;
}

/** Whether an entry survives the filter box: a case-insensitive substring of its
 * name, its label or its description. An empty box matches everything. */
export function matchesFilter(entry: MultiEntry, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return (
    entry.value.toLowerCase().includes(needle) ||
    entry.label.toLowerCase().includes(needle) ||
    entry.description.toLowerCase().includes(needle)
  );
}

/** The entries the filter box leaves. */
export function filterEntries(entries: MultiEntry[], query: string): MultiEntry[] {
  return entries.filter((e) => matchesFilter(e, query));
}

/** A tick or an untick. Ticking appends, so the saved order is the order the
 * admin built it in; unticking removes every copy, so a selection that arrived
 * with a duplicate cannot survive being turned off. */
export function toggleValue(selected: string[], value: string, on: boolean): string[] {
  if (!on) return selected.filter((v) => v !== value);
  return selected.includes(value) ? selected : [...selected, value];
}

/** What "All" selects: everything the menu is showing, the flagged tail
 * included. "All" means the list in front of the admin — quietly dropping a
 * stale name would be an edit nobody asked for, and it is one untick away. */
export function allValues(entries: MultiEntry[]): string[] {
  return entries.map((e) => e.value);
}

/** A multi-select box. `options` is what the server offers, `selected` is what
 * the application stores, and `onChange` gets the new selection. */
export function MultiSelect({
  id,
  options,
  selected,
  onChange,
  placeholder = "None",
  emptyText = "Nothing to choose from.",
}: {
  id: string;
  options: MultiChoice[];
  selected: string[];
  onChange: (selected: string[]) => void;
  /** Shown in the closed box when nothing is chosen. */
  placeholder?: string;
  /** Shown in the menu when the server offered nothing and nothing is stored. */
  emptyText?: string;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const box = useRef<HTMLDivElement>(null);

  // A menu that stays open behind the next thing clicked is a menu covering the
  // rest of the form, so an outside click and Escape both close it. The listener
  // exists only while it is open.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const entries = mergeChoices(options, selected);
  const shown = filterEntries(entries, query);
  const chosen = entries.filter((e) => selected.includes(e.value));

  const toggleKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      setOpen((o) => !o);
    }
  };

  return (
    <div className="multiselect" ref={box}>
      <div
        id={id}
        className="form-select multiselect-toggle"
        role="button"
        tabIndex={0}
        aria-expanded={open}
        aria-haspopup="listbox"
        onClick={() => setOpen((o) => !o)}
        onKeyDown={toggleKey}
      >
        {chosen.length === 0 && <span className="text-muted">{placeholder}</span>}
        {chosen.map((e) => (
          <span
            key={e.value}
            className={`badge multiselect-chip ${e.missing ? "bg-danger-lt" : "bg-blue-lt"}`}
          >
            {e.label}
            {/* Unpicking from the closed box, so removing one of eight does not
                mean opening the menu and hunting for it. `stopPropagation` keeps
                the click off the toggle behind it. */}
            <button
              type="button"
              className="btn-close ms-1"
              aria-label={`Remove ${e.label}`}
              onClick={(ev) => {
                ev.stopPropagation();
                onChange(toggleValue(selected, e.value, false));
              }}
            />
          </span>
        ))}
      </div>

      {open && (
        <div className="dropdown-menu show multiselect-menu">
          <div className="px-3 pb-2">
            <Form.Control
              size="sm"
              value={query}
              placeholder="Filter…"
              autoFocus
              aria-label="Filter the choices"
              onChange={(e) => setQuery(e.target.value)}
            />
            <div className="d-flex align-items-center gap-2 mt-2">
              <Button
                size="sm"
                variant="outline-secondary"
                onClick={() => onChange(allValues(entries))}
              >
                All
              </Button>
              <Button size="sm" variant="outline-secondary" onClick={() => onChange([])}>
                None
              </Button>
              <span className="text-muted small ms-auto">
                {selected.length} of {entries.length}
              </span>
            </div>
          </div>
          <div className="dropdown-divider" />
          <div className="multiselect-options px-3">
            {entries.length === 0 && <div className="text-muted small py-1">{emptyText}</div>}
            {entries.length > 0 && shown.length === 0 && (
              <div className="text-muted small py-1">Nothing matches “{query.trim()}”.</div>
            )}
            {shown.map((e) => (
              <Form.Check
                key={e.value}
                type="checkbox"
                id={`${id}-${e.value}`}
                className="mb-1"
                checked={selected.includes(e.value)}
                onChange={(ev) => onChange(toggleValue(selected, e.value, ev.target.checked))}
                label={
                  <>
                    <span className={e.missing ? "fw-semibold text-danger" : "fw-semibold"}>
                      {e.label}
                    </span>
                    {e.description && <div className="text-muted small">{e.description}</div>}
                    {e.missing && (
                      <div className="text-danger small">
                        Not on this server — this application will not mount until it is
                        removed.
                      </div>
                    )}
                  </>
                }
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
