/**
 * The settings screen's model: sections in, one payload out.
 *
 * The screen renders whatever the server declares, so what is worth pinning
 * down is the **round trip** — what an admin who opens Settings, changes one
 * box and saves actually sends. Two ways that can go wrong without a browser
 * noticing:
 *
 * - a value typed in a form is a string, and a `bool` or `int` setting sent as
 *   `"true"` or `"8443"` is refused by the server's own type check;
 * - the private key is shown as the redaction sentinel, and *not* sending it
 *   back unchanged would clear the stored certificate's key on the next save of
 *   an unrelated setting.
 */

import { describe, expect, it } from "vitest";

import { SECRET_SENTINEL, buildConfig, initialValues, readConfig } from "../settings";
import { allFields, settingsPayload } from "./Settings";

/** The shape `getSettings` returns, trimmed to what the model reads. */
const sections = [
  {
    name: "ssl",
    label: "SSL / TLS certificates",
    description: "How this server obtains its certificates.",
    fields: [
      {
        name: "ssl_mode",
        label: "Certificate source",
        type: "text",
        required: false,
        default: "off",
        options: ["off", "letsencrypt", "custom"],
        multiline: false,
        secret: false,
        create_only: false,
        help: "off serves plain HTTP.",
      },
      {
        name: "ssl_private_key",
        label: "Private key (PEM)",
        type: "text",
        required: false,
        default: null,
        options: [],
        multiline: true,
        secret: true,
        create_only: false,
        help: "Stored in the database.",
      },
      {
        name: "https_port",
        label: "HTTPS port",
        type: "int",
        required: false,
        default: 443,
        options: [],
        multiline: false,
        secret: false,
        create_only: false,
        help: "",
      },
      {
        name: "redirect_http_to_https",
        label: "Redirect HTTP to HTTPS",
        type: "bool",
        required: false,
        default: true,
        options: [],
        multiline: false,
        secret: false,
        create_only: false,
        help: "",
      },
    ],
  },
];

describe("the settings screen's model", () => {
  it("edits every section's fields as one payload", () => {
    expect(allFields(sections).map((f) => f.name)).toEqual([
      "ssl_mode",
      "ssl_private_key",
      "https_port",
      "redirect_http_to_https",
    ]);
  });

  it("sends each setting as the type its declaration says", () => {
    const stored = readConfig({
      ssl_mode: "custom",
      ssl_private_key: SECRET_SENTINEL,
      https_port: 8443,
      redirect_http_to_https: false,
    });
    const values = initialValues(allFields(sections), stored);
    // What the form holds is text — that is what an input is.
    expect(values.https_port).toBe("8443");
    expect(values.redirect_http_to_https).toBe("false");

    const payload = buildConfig(allFields(sections), values);
    expect(payload).toEqual({
      ssl_mode: "custom",
      // Untouched: the sentinel goes back, and the server reads it as "keep
      // what is stored". Anything else here would clear a working key.
      ssl_private_key: SECRET_SENTINEL,
      https_port: 8443,
      redirect_http_to_https: false,
    });
  });

  it("falls back to each declared default for a setting nobody has set", () => {
    const values = initialValues(allFields(sections), readConfig({}));
    expect(values.ssl_mode).toBe("off");
    expect(values.https_port).toBe("443");
    const payload = buildConfig(allFields(sections), values);
    expect(payload.https_port).toBe(443);
    // An empty optional box is left out rather than sent as "", which is what
    // makes clearing a setting mean "use the default".
    expect(payload).not.toHaveProperty("ssl_private_key");
  });

  /** Each setting is its own row, so an omitted key means "leave it" — which
   * would make an emptied box do nothing. `null` is what clears one. */
  it("clears an emptied setting rather than leaving it as it was", () => {
    const values = initialValues(
      allFields(sections),
      readConfig({ ssl_mode: "custom", ssl_extra_domains: "old.example.com" }),
    );
    values.ssl_private_key = "";
    const payload = settingsPayload(allFields(sections), values);
    expect(payload.ssl_private_key).toBeNull();
    // Everything else still travels as its declared type.
    expect(payload.ssl_mode).toBe("custom");
    expect(payload.https_port).toBe(443);
  });

  it("sends a replaced secret as what was typed", () => {
    const values = initialValues(
      allFields(sections),
      readConfig({ ssl_private_key: SECRET_SENTINEL }),
    );
    values.ssl_private_key = "-----BEGIN PRIVATE KEY-----\nnew\n-----END PRIVATE KEY-----";
    expect(buildConfig(allFields(sections), values).ssl_private_key).toContain("new");
  });
});
