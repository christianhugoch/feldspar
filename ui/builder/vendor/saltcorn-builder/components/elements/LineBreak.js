// Vendored from Saltcorn 1: packages/saltcorn-builder/src/components/elements/LineBreak.js
// at @saltcorn/builder 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/builder/vendor/README.md.
/**
 * @category saltcorn-builder
 * @module components/elements/LineBreak
 * @subcategory components / elements
 */

import React, { Fragment } from "react";
import { useNode } from "@craftjs/core";
import { SettingsFromFields } from "./utils";

export /**
 * @param {object} [props = {}]
 * @returns {Fragment}
 * @namespace
 * @category saltcorn-builder
 * @subcategory components
 */
const LineBreak = ({ hr, page_break_after }) => {
  const {
    selected,
    connectors: { connect, drag },
  } = useNode((node) => ({ selected: node.events.selected }));
  return hr || page_break_after ? (
    <hr></hr>
  ) : (
    <Fragment>
      <span
        className={selected ? "selected-node" : ""}
        ref={(dom) => connect(drag(dom))}
      >
        ↵
      </span>
      <br />
    </Fragment>
  );
};

const fields = [
  { label: "Page break", name: "page_break_after", type: "Bool" },
  { label: "Horizontal rule", name: "hr", type: "Bool" },
];

/**
 * @type {object}
 */
LineBreak.craft = {
  displayName: "LineBreak",
  related: {
    settings: SettingsFromFields(fields),
    segment_type: "line_break",
    fields,
  },
};
