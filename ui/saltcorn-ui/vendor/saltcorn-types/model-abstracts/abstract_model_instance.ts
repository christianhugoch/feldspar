// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_model_instance.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_model_instance
 * @subcategory model-abstracts
 */

/** A trained instance of a ModelCfg (model-abstracts/abstract_model). */
export type ModelInstanceCfg = {
  id?: number;
  name: string;
  model_id: number;
  state: any;
  hyperparameters: any;
  trained_on: Date;
  report: string;
  metric_values: any;
  parameters: any;
  fit_object: Buffer;
  is_default?: boolean;
};

/** A portable (import/export) representation of a {@link ModelInstanceCfg}. */
export type ModelInstancePack = {
  model_name: string;
  table_name: string;
} & Omit<ModelInstanceCfg, "model_id" | "id">;
