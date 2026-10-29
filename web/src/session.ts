// ==============================================================================
// The signed-in account
// ==============================================================================
//
// A session is the bearer token from sign-in (spec 4.1) plus the ids the client
// stamps on its writes. It lives in localStorage, so a reload or a new tab stays
// signed in. A 401 on any request ends it; the username is kept so the sign-in
// form can offer it again.

import { createSignal } from "solid-js";
import { type RequestOptions, request, ApiError } from "./api/http";
import type { TokenResponse } from "./api/types";
import { deviceName } from "./device";

export type Session = {
  token: string;
  deviceId: string;
  userId: string;
  username: string;
};

const SESSION_KEY = "pod.session";
const LAST_USERNAME_KEY = "pod.lastUsername";

function load(): Session | null {
  try {
    const stored = JSON.parse(localStorage.getItem(SESSION_KEY) ?? "null");
    return typeof stored?.token === "string" ? stored : null;
  } catch {
    return null;
  }
}

function store(session: Session | null) {
  try {
    if (session) {
      localStorage.setItem(SESSION_KEY, JSON.stringify(session));
      localStorage.setItem(LAST_USERNAME_KEY, session.username);
    } else {
      localStorage.removeItem(SESSION_KEY);
    }
  } catch {
    // Storage can be off (a private window, blocked site data). The session
    // then lasts as long as the tab.
  }
  setSession(session);
}

const [session, setSession] = createSignal<Session | null>(load());
const [expired, setExpired] = createSignal(false);

export { session };

/** Whether the last session ended with a 401 rather than a sign-out. */
export { expired };

export function lastUsername(): string {
  try {
    return localStorage.getItem(LAST_USERNAME_KEY) ?? "";
  } catch {
    return "";
  }
}

export async function signIn(username: string, password: string, register: boolean): Promise<void> {
  const response = await request<TokenResponse>("POST", register ? "/v1/auth/register" : "/v1/auth/login", {
    body: { username, password, device_name: deviceName(navigator.userAgent) },
  });
  setExpired(false);
  store({
    token: response.token,
    deviceId: response.device_id,
    userId: response.user_id,
    // The server keys accounts on the trimmed, lowercased name (spec 4.1).
    username: username.trim().toLowerCase(),
  });
}

/** Ends the session on the server too. Signing out works offline; the token then just expires. */
export async function signOut(): Promise<void> {
  const current = session();
  store(null);
  if (current) await request("POST", "/v1/auth/logout", { token: current.token }).catch(() => {});
}

/** An authenticated request. A 401 signs the tab out before the error reaches the caller. */
export async function api<T>(method: string, path: string, options: Omit<RequestOptions, "token"> = {}): Promise<T> {
  const current = session();
  if (!current) throw new ApiError(401, "unauthenticated", "Signed out.");
  try {
    return await request<T>(method, path, { ...options, token: current.token });
  } catch (error) {
    // Only the session that sent the request may be ended by its answer; a
    // sign-in may have happened while it was in flight.
    if (error instanceof ApiError && error.status === 401 && session()?.token === current.token) {
      setExpired(true);
      store(null);
    }
    throw error;
  }
}
