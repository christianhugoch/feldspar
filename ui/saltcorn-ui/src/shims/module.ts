// `utils.ts` builds its own `require` from `node:module` to reach `db/index`
// and `db/state` lazily (a static import would be a load-time cycle in v1).
// Those two are host-supplied here, so its `require` answers them through the
// host's; anything else it asks for is a path that only exists in v1's tree.
export function createRequire(_from: string) {
  return (specifier: string): unknown => {
    switch (specifier) {
      case "./db/index.js":
        return require("@saltcorn/data/db/index");
      case "./db/state.js":
        return require("@saltcorn/data/db/state");
      default:
        throw new Error(
          `@saltcorn/data/utils asked for ${specifier}, which the Saltcorn UI view runtime does not have`,
        );
    }
  };
}

export default { createRequire };
