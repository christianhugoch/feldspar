// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_library.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_library
 * @subcategory model-abstracts
 */

/** A reusable Library component's configuration. */
export type LibraryCfg = {
  id?: number;
  name: string;
  icon: string;
  layout: string | any;
};

/** A portable (import/export) representation of a {@link LibraryCfg}. */
export type LibraryPack = {} & LibraryCfg;
