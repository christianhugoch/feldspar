// Vendored from Saltcorn 1: packages/common-code/index.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
import { Relation } from "./relations/relation.js";
export { Relation };

import { RelationsFinder } from "./relations/relations_finder.js";
export { RelationsFinder };

import { ViewDisplayType, RelationType } from "./relations/relation_types.js";
export { ViewDisplayType, RelationType };

import {
  parseRelationPath,
  parseLegacyRelation,
  buildRelationPath,
  buildTableCaches,
} from "./relations/relation_helpers.js";
export {
  parseRelationPath,
  parseLegacyRelation,
  buildRelationPath,
  buildTableCaches,
};
// TODO when we add more then we need namspaces that work with node, webpack and jest
