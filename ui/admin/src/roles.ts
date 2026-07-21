// Roles as the admin picks them.
//
// A role is an integer on the fixed 1–100 scale (design §7.1), of which only the
// two ends have meanings of their own: 1 is admin, 100 is public. The server
// answers `listRoles` with those two plus every role its users actually hold —
// see the endpoint's own note for why that set and not all hundred — and this
// module is the client half: load the list once per screen, and render a role
// number as something an admin can read.

import { useEffect, useState } from "react";

import { api } from "./api";
import type { ListRolesResponse } from "./client";

/** The role choices, or `null` while they are still loading. */
export type Roles = ListRolesResponse | null;

/** Load the role choices once. Failure yields an empty list, not a crash. */
export function useRoles(): Roles {
  const [roles, setRoles] = useState<Roles>(null);
  useEffect(() => {
    void (async () => {
      try {
        setRoles(await api.listRoles());
      } catch {
        setRoles([]);
      }
    })();
  }, []);
  return roles;
}

/**
 * A role rendered for a human: its name and number, or just its number when the
 * server did not name it.
 */
export function roleLabel(role: number, roles: Roles): string {
  const named = roles?.find((r) => r.role === role);
  return named ? `${named.label} (${role})` : `Role ${role}`;
}

/**
 * The choices to offer for a role select, with `current` included even when the
 * server did not list it.
 *
 * A table configured for role 40 before the last user of role 40 was deleted
 * must still show 40 as its current value — a select that silently snapped to
 * the nearest listed role would change who can reach the data the next time the
 * form was saved, and would do it without saying so.
 */
export function roleOptions(current: number, roles: Roles): { role: number; label: string }[] {
  const listed = roles ?? [];
  const options = listed.some((r) => r.role === current)
    ? [...listed]
    : [...listed, { role: current, label: `Role ${current}` }];
  return options.sort((a, b) => a.role - b.role);
}
