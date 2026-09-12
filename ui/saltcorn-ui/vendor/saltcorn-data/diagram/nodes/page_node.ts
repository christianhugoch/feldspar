// Vendored from Saltcorn 1: packages/saltcorn-data/diagram/nodes/page_node.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import { AbstractPage } from "@saltcorn/types/model-abstracts/abstract_page";
import { AbstractTag } from "@saltcorn/types/model-abstracts/abstract_tag";
import Node from "./node.js";

export class PageNode extends Node {
  page: AbstractPage;

  constructor(page: AbstractPage, tags: Array<AbstractTag>) {
    super("page", page.name, page.name, tags, page.id!);
    this.page = page;
  }

  cyDataObject() {
    const result = this.commonCyData();
    result.min_role = this.page.min_role;
    return result;
  }
}
