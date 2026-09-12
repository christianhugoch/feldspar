// Vendored from Saltcorn 1: packages/saltcorn-data/diagram/nodes/view_node.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import { AbstractTag } from "@saltcorn/types/model-abstracts/abstract_tag";
import { AbstractView } from "@saltcorn/types/model-abstracts/abstract_view";
import Node from "./node.js";

export class ViewNode extends Node {
  view: AbstractView;

  constructor(view: AbstractView, tags: Array<AbstractTag>) {
    super("view", view.name, view.name, tags, view.id!);
    this.view = view;
  }

  cyDataObject() {
    const result = this.commonCyData();
    // @ts-ignore  TODO check table type
    result.table = this.view.table;
    result.viewtemplate = this.view.viewtemplate;
    result.min_role = this.view.min_role;
    return result;
  }
}
