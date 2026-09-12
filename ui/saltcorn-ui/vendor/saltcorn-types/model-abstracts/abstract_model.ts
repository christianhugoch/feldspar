// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_model.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_model
 * @subcategory model-abstracts
 */

/** Configuration for a machine-learning Model attached to a table. */
export type ModelCfg = {
  id?: number;
  name: string;
  table_id: number;
  modelpattern: string;
  configuration: any;
};

/** A portable (import/export) representation of a {@link ModelCfg}. */
export type ModelPack = {
  table_name: string;
} & Omit<ModelCfg, "id" | "table_id">;
