// `models/expression.ts` evaluates formulas with vm2's `VM` when it believes it
// is on Node, and with `vm.runInNewContext` otherwise. vm2 is not something a
// Deno worker can load, and v1's mobile mock of it throws — which would make
// every formula in a view fail. So `VM.run` is `runInNewContext`: the branch
// v1 itself takes off Node.
//
// The isolation this gives up is vm2's in-process sandbox. The boundary that
// matters here is the worker's: the view runtime runs as a module with an empty
// permission set (TODO §3), so a formula that escapes the context reaches a
// worker with no net, no fs and no env.
import { runInNewContext } from "vm";

export class VM {
  private sandbox: object;

  constructor(options: { sandbox?: object } = {}) {
    this.sandbox = options.sandbox ?? {};
  }

  run(code: string): unknown {
    return runInNewContext(code, this.sandbox);
  }
}

export default { VM };
