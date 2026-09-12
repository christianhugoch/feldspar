// `utils.ts` imports `HttpsProxyAgent` for v1's outbound fetch proxy settings.
// A view never makes an outbound request from the view runtime, and the worker
// has no net permission, so constructing one is refused by name.
export class HttpsProxyAgent {
  constructor() {
    throw new Error(
      "https-proxy-agent is not available in the Saltcorn UI view runtime: views make no outbound requests",
    );
  }
}

export default { HttpsProxyAgent };
