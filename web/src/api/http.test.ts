import { afterEach, expect, test, vi } from "vitest";
import { ApiError, request } from "./http";

afterEach(() => vi.unstubAllGlobals());

function answer(status: number, body?: unknown) {
  const fetch = vi.fn(async () => new Response(body === undefined ? null : JSON.stringify(body), { status }));
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

test("sends the token and a JSON body", async () => {
  const fetch = answer(200, { ok: true });
  expect(await request("POST", "/v1/sync", { token: "t0k", body: { mutations: [] } })).toEqual({ ok: true });
  const [path, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
  expect(path).toBe("/v1/sync");
  expect(init.headers).toEqual({ Authorization: "Bearer t0k", "Content-Type": "application/json" });
  expect(init.body).toBe('{"mutations":[]}');
});

test("a 204 has no body", async () => {
  answer(204);
  expect(await request("POST", "/v1/auth/logout")).toBeUndefined();
});

test("an error body becomes an ApiError with its code and message", async () => {
  answer(401, { error: { code: "invalid_credentials", message: "Username or password is wrong." } });
  const error = await request<never>("POST", "/v1/auth/login").catch((e: ApiError) => e);
  expect(error).toBeInstanceOf(ApiError);
  expect(error).toMatchObject({ status: 401, code: "invalid_credentials", message: "Username or password is wrong." });
  expect(error.retryable).toBe(false);
});

test("a body that isn't the error shape still reports the status", async () => {
  vi.stubGlobal("fetch", async () => new Response("<html>Bad Gateway</html>", { status: 502 }));
  const error = await request<never>("GET", "/v1/inbox").catch((e: ApiError) => e);
  expect(error).toMatchObject({ status: 502, code: "unknown" });
  expect(error.retryable).toBe(true);
});

test("no answer is status 0, retryable", async () => {
  vi.stubGlobal("fetch", async () => {
    throw new TypeError("Failed to fetch");
  });
  const error = await request<never>("GET", "/v1/inbox").catch((e: ApiError) => e);
  expect(error).toMatchObject({ status: 0, code: "network", retryable: true });
});
