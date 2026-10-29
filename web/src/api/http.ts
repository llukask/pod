// ==============================================================================
// Requests to pod-server
// ==============================================================================
//
// The client is served by the server it talks to, so every path is relative to
// this origin (`/v1/...`). Errors arrive in one shape (spec 4) and become an
// `ApiError`; a request that never got an answer is status 0, code `network`.

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }

  /**
   * Whether trying again later can succeed: no answer, `429` or a `5xx`. The
   * code set is open, so this goes by status alone (spec 4).
   */
  get retryable(): boolean {
    return this.status === 0 || this.status === 429 || this.status >= 500;
  }
}

export type RequestOptions = {
  token?: string;
  body?: unknown;
  signal?: AbortSignal;
  /** Lets the request outlive the page (`fetch` `keepalive`), for the writes sent on `pagehide`. */
  keepalive?: boolean;
};

export async function request<T>(method: string, path: string, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = {};
  if (options.token) headers.Authorization = `Bearer ${options.token}`;
  if (options.body !== undefined) headers["Content-Type"] = "application/json";

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: options.signal,
      keepalive: options.keepalive,
    });
  } catch (error) {
    if (options.signal?.aborted) throw error;
    throw new ApiError(0, "network", "Can't reach the server.");
  }

  if (response.ok) {
    return (response.status === 204 ? undefined : await response.json()) as T;
  }
  // A proxy in front of the server can answer with its own page, so a body
  // that isn't the error shape still becomes an error with the status.
  const body = await response.json().catch(() => null);
  const error = body?.error;
  throw typeof error?.code === "string"
    ? new ApiError(response.status, error.code, String(error.message ?? error.code))
    : new ApiError(response.status, "unknown", `The server answered ${response.status}.`);
}
