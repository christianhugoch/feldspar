// Vendored from Saltcorn 1: packages/saltcorn-markup/workflow.ts
// at @saltcorn/data 1.7.0-alpha.1 (saltcorn/saltcorn 0508c45ac2). Do not edit; see ui/saltcorn-ui/vendor/README.md.
/**
 * @category saltcorn-markup
 * @module workflow
 */

import tags from "./tags.js";
const { div, script, style } = tags;

const encode = (x: any): string => encodeURIComponent(JSON.stringify(x));

/**
 * Render the workflow editor shell
 * @param workflowData
 * @param version_tag
 * @returns {string}
 */
const renderWorkflow = (workflowData: any, version_tag?: string): string =>
  div(
    { class: "workflow-editor-wrapper" },
    style(/*css*/ `
      .workflow-editor-wrapper {
        display: flex;
        flex-direction: column;
      }
      #saltcorn-workflow-editor {
        flex: 1;
        min-height: 0px;
      }
    `),
    script({
      src: version_tag
        ? `/static_assets/${version_tag}/workflow_bundle.js`
        : "/workflow_bundle.js",
    }),
    div({ id: "saltcorn-workflow-editor" }),
    script(
      `workflow.renderWorkflowEditor("saltcorn-workflow-editor", "${encode(workflowData)}");`
    )
  );

export default renderWorkflow;
