// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_event_log.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_event_log
 * @subcategory model-abstracts
 */

/** A portable (import/export) representation of a logged event. */
export type EventLogPack = {
  event_type: string;
  channel?: string | null;
  occur_at: Date;
  user_email?: string | null;
  payload?: any;
  email?: string;
};
