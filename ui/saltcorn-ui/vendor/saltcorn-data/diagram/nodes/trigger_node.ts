// Vendored from Saltcorn 1: packages/saltcorn-data/diagram/nodes/trigger_node.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import { AbstractTag } from "@saltcorn/types/model-abstracts/abstract_tag";
import Node from "./node.js";

export class TriggerNode extends Node {
  constructor(
    name: string,
    label: string,
    tags: Array<AbstractTag>,
    objectId?: number | null
  ) {
    super("trigger", name, label, tags, objectId);
  }

  cyDataObject() {
    const result = this.commonCyData();
    result.isVirtual = this.objectId ? false : true;
    return result;
  }
}
