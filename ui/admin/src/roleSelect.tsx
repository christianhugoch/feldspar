// The one way an admin picks a minimum role.
//
// A role is a number on the 1–100 scale, but the number is not the thing being
// chosen — the role is, and only the server knows which numbers exist and what
// they are called. A free-text number box let an admin type a role nobody had
// defined, and told them nothing about which number meant which role, so every
// minimum-role input on every screen is this select over `useRoles`.
//
// Two shapes, because the two cases differ in what an empty value means and it
// is worth making that a type error rather than a convention: `RoleSelect` for
// a field that must name a role (a table's read rule), `OptionalRoleSelect` for
// one that may name none (an agent, whose blank means admin only, or a file,
// whose blank means no rule here).

import type { ReactNode } from "react";
import Form from "react-bootstrap/Form";

import { roleOptions, type Roles } from "./roles";

type Common = {
  id: string;
  label: string;
  roles: Roles;
  /** Overrides the default `mb-3`, for a select in a tighter row. */
  className?: string;
  /** Help text below the select. */
  children?: ReactNode;
};

/** A select over the roles the server offers, with the current value included. */
export function RoleSelect({
  value,
  onChange,
  ...rest
}: Common & { value: number; onChange: (role: number) => void }) {
  return (
    <Group {...rest} value={value} onChange={(role) => onChange(role as number)} />
  );
}

/**
 * The same select for a role that may be unset, with `blank` naming what having
 * no role means here — it is never the same sentence twice, and it is the whole
 * meaning of the choice.
 */
export function OptionalRoleSelect({
  value,
  blank,
  onChange,
  ...rest
}: Common & {
  value: number | null;
  blank: string;
  onChange: (role: number | null) => void;
}) {
  return <Group {...rest} value={value} blank={blank} onChange={onChange} />;
}

function Group({
  id,
  label,
  value,
  roles,
  blank,
  className,
  children,
  onChange,
}: Common & {
  value: number | null;
  blank?: string;
  onChange: (role: number | null) => void;
}) {
  return (
    <Form.Group className={className ?? "mb-3"} controlId={id}>
      <Form.Label>{label}</Form.Label>
      <Form.Select
        value={value == null ? "" : String(value)}
        onChange={(e) => onChange(e.target.value === "" ? null : Number(e.target.value))}
      >
        {blank !== undefined && <option value="">{blank}</option>}
        {roleOptions(value, roles).map((option) => (
          <option key={option.role} value={option.role}>
            {option.name} ({option.role})
          </option>
        ))}
      </Form.Select>
      {children != null && <Form.Text muted>{children}</Form.Text>}
    </Form.Group>
  );
}
