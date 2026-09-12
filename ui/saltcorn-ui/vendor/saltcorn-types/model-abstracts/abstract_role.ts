// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_role.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_role
 * @subcategory model-abstracts
 */

/** A user role, e.g. Admin, Staff, User, Public. */
export interface AbstractRole {
  id: number;
  role: string;
}

/** Configuration shape for creating/updating a role. */
export type RoleCfg = AbstractRole

/** A portable (import/export) representation of a {@link RoleCfg}. */
export type RolePack = {} & RoleCfg;
