// Vendored from Saltcorn 1: packages/saltcorn-data/diagram/nodes/dummy_node.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import Node from "./node.js";

export class DummyNode extends Node {
  constructor() {
    super("dummy", "dummy", "dummy", [], -1);
  }

  cyDataObject() {
    return {};
  }
}
