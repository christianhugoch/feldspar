// Vendored from Saltcorn 1: packages/saltcorn-data/diagram/nodes/table_node.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import { AbstractTable } from "@saltcorn/types/model-abstracts/abstract_table";
import { AbstractField } from "@saltcorn/types/model-abstracts/abstract_field";
import { AbstractTag } from "@saltcorn/types/model-abstracts/abstract_tag";
import Node from "./node.js";

export class TableNode extends Node {
  table: AbstractTable;

  constructor(table: AbstractTable, tags: Array<AbstractTag>) {
    super("table", table.name, table.name, tags, table.id!);
    this.table = table;
  }

  cyDataObject() {
    const result = this.commonCyData();
    if (this.table.fields) {
      result.fields = this.table.fields.map((field: AbstractField) => {
        return { name: field.name, typeName: field.pretty_type };
      });
    }
    return result;
  }
}
