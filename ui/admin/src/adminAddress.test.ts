/**
 * Following the admin UI to a new address, and linking to applications once it
 * has moved.
 *
 * What is worth pinning is what the admin would otherwise get wrong without
 * noticing: a Go button enabled before the new address can be reached (a
 * browser certificate error, and an admin locked out of the screen that caused
 * it), and links to applications built on the admin's own host once that host
 * is `admin.example.com` (`blog.admin.example.com` resolves to nothing).
 */

import { describe, expect, it } from "vitest";

import {
  adminMoved,
  answers,
  appHost,
  appUrl,
  moveSteps,
  originOf,
  readyToFollow,
} from "./adminAddress";
import type { GetAdminAddressResponse } from "./client";

const here = { protocol: "https:", host: "example.com" };
const dev = { protocol: "http:", host: "localhost:3032" };

function address(
  certificate: GetAdminAddressResponse["certificate"],
): GetAdminAddressResponse {
  return {
    base_domain: "example.com",
    admin_subdomain: "admin",
    admin_host: "admin.example.com",
    ready: certificate == null || ["ready", "plain_http"].includes(certificate.state),
    certificate,
  };
}

describe("adminMoved", () => {
  it("is a change in the stored subdomain, however it was typed", () => {
    expect(adminMoved({}, { admin_subdomain: "admin" })).toBe(true);
    expect(adminMoved({ admin_subdomain: "admin" }, { admin_subdomain: "" })).toBe(true);
    expect(adminMoved({ admin_subdomain: "admin" }, { admin_subdomain: " Admin " })).toBe(false);
    expect(adminMoved({ log_sql: "true" }, { log_sql: "false" })).toBe(false);
  });
});

describe("originOf", () => {
  it("keeps the scheme and the port the page was reached on", () => {
    expect(originOf("admin.example.com", here)).toBe("https://admin.example.com");
    expect(originOf("admin.localhost", dev)).toBe("http://admin.localhost:3032");
  });
});

describe("moveSteps", () => {
  it("waits for the certificate before it tries the address", () => {
    const steps = moveSteps(
      address({ state: "ordering", message: "the CA could not connect" }),
      null,
    );
    expect(steps.map((s) => s.state)).toEqual(["done", "active", "pending"]);
    expect(steps[1].detail).toBe("the CA could not connect");
    expect(readyToFollow(steps)).toBe(false);
  });

  it("then waits for the address to answer this browser", () => {
    const tried = moveSteps(address({ state: "ready", message: null }), false);
    expect(tried.map((s) => s.state)).toEqual(["done", "done", "active"]);
    expect(tried[2].detail).toMatch(/DNS/);
    expect(readyToFollow(tried)).toBe(false);

    const reached = moveSteps(address({ state: "ready", message: null }), true);
    expect(readyToFollow(reached)).toBe(true);
  });

  it("needs no certificate over plain HTTP", () => {
    const steps = moveSteps(address({ state: "plain_http", message: null }), true);
    expect(steps[1].label).toMatch(/plain HTTP/);
    expect(readyToFollow(steps)).toBe(true);
  });

  it("says so when no certificate will cover the name, and never lets the admin go", () => {
    const steps = moveSteps(
      address({ state: "not_covered", message: "the pasted certificate does not name it" }),
      true,
    );
    expect(steps[1].state).toBe("failed");
    expect(steps[2].state).toBe("pending");
    expect(readyToFollow(steps)).toBe(false);
  });

  it("is all waiting before the server has answered", () => {
    expect(readyToFollow(moveSteps(null, null))).toBe(false);
  });
});

describe("answers", () => {
  it("is any reply at all, and no reply is a refusal", async () => {
    const asked: string[] = [];
    const reply = (async (url: string) => {
      asked.push(url);
      return new Response(null);
    }) as unknown as typeof fetch;
    expect(await answers("https://admin.example.com", reply)).toBe(true);
    expect(asked).toEqual(["https://admin.example.com/health"]);

    const refuse = (async () => {
      throw new TypeError("certificate not trusted");
    }) as unknown as typeof fetch;
    expect(await answers("https://admin.example.com", refuse)).toBe(false);
  });
});

describe("appHost", () => {
  it("is a subdomain of the base domain, not of the admin's own host", () => {
    const admin = { protocol: "https:", host: "admin.example.com" };
    expect(appHost("blog", admin, "example.com")).toBe("blog.example.com");
    expect(appUrl("blog", admin, "example.com")).toBe("https://blog.example.com");
  });

  it("is the base domain itself for the application at @", () => {
    const admin = { protocol: "http:", host: "admin.localhost:3032" };
    expect(appUrl("@", admin, "localhost")).toBe("http://localhost:3032");
    expect(appUrl("blog", admin, "localhost")).toBe("http://blog.localhost:3032");
  });

  it("falls back to the admin's own host before the server has said", () => {
    expect(appUrl("blog", dev, null)).toBe("http://blog.localhost:3032");
  });
});
