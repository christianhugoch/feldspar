// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_workflow_run.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_workflow_run
 * @subcategory model-abstracts
 */

/** A single in-progress or completed execution of a workflow trigger. */
export type WorkflowRunCfg = {
  id?: number;
  trigger_id: number;
  context?: any;
  wait_info?: any;
  started_at?: Date;
  status_updated_at?: Date;
  started_by?: number;
  error?: string;
  session_id?: string;
  status?: "Pending" | "Running" | "Finished" | "Waiting" | "Error";
  current_step?: any[];
};
