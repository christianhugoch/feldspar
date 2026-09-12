// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_user.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_user
 * @subcategory model-abstracts
 */

/** A logged-in (or loggable) Saltcorn user. */
export interface AbstractUser {
  email?: string;
  role_id: number;
  id?: number;
  [k: string]: any;
}

/** Carries the user (or Public) a request should be evaluated as. */
export interface ForUserRequest {
  forUser?: AbstractUser;
  forPublic?: boolean;
}
