// Vendored from Saltcorn 1: packages/saltcorn-types/model-abstracts/abstract_tag_entry.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-types
 * @module model-abstracts/abstract_tag_entry
 * @subcategory model-abstracts
 */

/** One tagged object (table, view, page, or trigger) within an AbstractTag (model-abstracts/abstract_tag). */
export interface AbstractTagEntry {
  id?: number;
  tag_id?: number;
  table_id?: number;
  view_id?: number;
  page_id?: number;
  trigger_id?: number;
}

/** A portable (import/export) representation of an {@link AbstractTagEntry}. */
export type TagEntryPack = {
  table_name?: string;
  view_name?: string;
  page_name?: string;
  trigger_name?: string;
};
