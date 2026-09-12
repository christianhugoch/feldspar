// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_workflow_trace.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_workflow_trace
 * @subcategory model-abstracts
 */

/** A recorded execution of one step within a WorkflowRunCfg (model-abstracts/abstract_workflow_run). */
export type WorkflowTraceCfg = {
  id?: number;
  run_id: number;
  context: any;
  step_name_run: string;
  wait_info?: any;
  step_started_at: Date;
  elapsed: number;
  user_id?: number;
  error?: string;
  status: "Pending" | "Running" | "Finished" | "Waiting" | "Error";
};
