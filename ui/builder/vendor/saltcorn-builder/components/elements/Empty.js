// Vendored from Saltcorn 1: packages/saltcorn-builder/src/components/elements/Empty.js
// at @saltcorn/builder 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/builder/vendor/README.md.
/**
 * @category saltcorn-builder
 * @module components/elements/Empty
 * @subcategory components / elements
 */

import React, { Fragment } from "react";
import { useNode } from "@craftjs/core";

export /**
 * @param {object} [props = {}]
 * @returns {Fragment}
 * @namespace
 * @category saltcorn-builder
 * @subcategory components
 */
const Empty = () => {
  const {
    selected,
    connectors: { connect, drag },
  } = useNode((node) => ({ selected: node.events.selected }));
  return null;
};

/**
 * @type {object}
 */
Empty.craft = {
  displayName: "Empty",
};
