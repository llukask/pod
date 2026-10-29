import { afterEach, beforeEach, expect, test, vi } from "vitest";

// The session signal reads localStorage when the module loads, so each test
// imports a fresh copy.
async function fresh() {
  vi.resetModules();
  return await import("./session");
}

function respond(handler: (path: string, init: RequestInit) => [number, unknown?]) {
  vi.stubGlobal("fetch", async (path: string, init: RequestInit) => {
    const [status, body] = handler(path, init);
    return new Response(body === undefined ? null : JSON.stringify(body), { status });
  });
}

const token = (name: string) => ({ token: name, device_id: "dev-1", user_id: "user-1", expires_at: "2027-01-01T00:00:00Z" });

beforeEach(() => localStorage.clear());
afterEach(() => vi.unstubAllGlobals());

test("signing in stores the session across reloads", async () => {
  respond(() => [200, token("t1")]);
  const { signIn, session } = await fresh();
  await signIn("  Alice ", "hunter222", false);
  expect(session()).toEqual({ token: "t1", deviceId: "dev-1", userId: "user-1", username: "alice" });
  expect((await fresh()).session()?.token).toBe("t1");
});

test("a 401 ends the session and keeps the username", async () => {
  respond((path) => (path === "/v1/auth/login" ? [200, token("t1")] : [401, { error: { code: "unauthenticated", message: "Expired." } }]));
  const { signIn, api, session, expired, lastUsername } = await fresh();
  await signIn("alice", "hunter222", false);
  await expect(api("GET", "/v1/inbox")).rejects.toMatchObject({ status: 401 });
  expect(session()).toBeNull();
  expect(expired()).toBe(true);
  expect(lastUsername()).toBe("alice");
});

test("a 401 for an older session leaves a newer one alone", async () => {
  let release: () => void = () => {};
  vi.stubGlobal("fetch", async (path: string) => {
    if (path === "/v1/auth/login") return new Response(JSON.stringify(token(`t${Math.random()}`)), { status: 200 });
    await new Promise<void>((resolve) => (release = resolve));
    return new Response(JSON.stringify({ error: { code: "unauthenticated", message: "" } }), { status: 401 });
  });
  const { signIn, api, session } = await fresh();
  await signIn("alice", "hunter222", false);
  const stale = api("GET", "/v1/inbox").catch(() => {});
  await signIn("alice", "hunter222", false);
  const newer = session()?.token;
  release();
  await stale;
  expect(session()?.token).toBe(newer);
});

test("signing out clears the session even when the server can't be reached", async () => {
  respond(() => [200, token("t1")]);
  const { signIn, signOut, session, expired } = await fresh();
  await signIn("alice", "hunter222", false);
  vi.stubGlobal("fetch", async () => {
    throw new TypeError("offline");
  });
  await signOut();
  expect(session()).toBeNull();
  expect(expired()).toBe(false);
  expect(localStorage.getItem("pod.session")).toBeNull();
});
