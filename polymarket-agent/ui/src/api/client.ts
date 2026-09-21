/**
 * The HTTP boundary. Everything above this file deals in `ApiResult`, never in
 * fetch, status codes, or the shape quirks in `narrow.ts`.
 */
import { auth } from "./auth";
import type { Narrowed } from "./narrow";

export type ApiFailure =
  /** fetch() threw — agent stopped, network down, request aborted. */
  | { kind: "network"; message: string }
  /** 401. The middleware is only active when the server has a token set. */
  | { kind: "unauthorized" }
  /** Any other non-2xx. */
  | { kind: "http"; status: number; message: string }
  /** 2xx whose body was not JSON, or not the shape we expected. */
  | { kind: "malformed"; message: string }
  /**
   * HTTP 200 carrying `{"error": "..."}`. A real server-side failure wearing a
   * success status — promoted here so a card can show the agent's own message
   * instead of an empty table that looks like "nothing has happened yet".
   */
  | { kind: "handler"; message: string };

export type ApiResult<T> =
  | { ok: true; value: T }
  | { ok: false; error: ApiFailure };

export function failureText(f: ApiFailure): string {
  switch (f.kind) {
    case "network":
      return `Cannot reach the agent (${f.message})`;
    case "unauthorized":
      return "Not authorized — a dashboard token is required";
    case "http":
      return `Server returned ${f.status}${f.message ? ` — ${f.message}` : ""}`;
    case "malformed":
      return `Unexpected response — ${f.message}`;
    case "handler":
      return f.message;
  }
}

/**
 * Sent on every control request. Must match `CONTROL_HEADER` in
 * `monitoring/dashboard.rs`.
 */
export const CONTROL_HEADER = "x-agent-control";

/** A failure the operator can fix by supplying a token. */
export function isAuthFailure(f: ApiFailure): boolean {
  return f.kind === "unauthorized";
}

function toNarrowFailure<T>(n: Narrowed<T>): ApiFailure {
  if ("handlerError" in n) return { kind: "handler", message: n.handlerError };
  if ("malformed" in n) return { kind: "malformed", message: n.malformed };
  // Unreachable: callers only pass failures here.
  return { kind: "malformed", message: "unknown" };
}

interface GetOptions<T> {
  path: string;
  narrow: (body: unknown) => Narrowed<T>;
  signal?: AbortSignal;
}

async function attempt<T>(
  opts: GetOptions<T>,
  token: string,
): Promise<ApiResult<T> | { retryWithAuth: true }> {
  let res: Response;
  try {
    res = await fetch(opts.path, {
      signal: opts.signal,
      headers: token ? { Authorization: `Bearer ${token}` } : undefined,
      // The dashboard is same-origin in production and proxied in dev, so
      // credentials are never needed and cache is never wanted.
      cache: "no-store",
    });
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return { ok: false, error: { kind: "network", message } };
  }

  if (res.status === 401) {
    // The 401 body is text/plain "unauthorized"; calling .json() on it throws.
    return { retryWithAuth: true };
  }

  if (!res.ok) {
    const text = await res.text().catch(() => "");
    return {
      ok: false,
      error: { kind: "http", status: res.status, message: text.slice(0, 200) },
    };
  }

  let body: unknown;
  try {
    body = await res.json();
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return { ok: false, error: { kind: "malformed", message } };
  }

  const narrowed = opts.narrow(body);
  if (narrowed.ok) return { ok: true, value: narrowed.value };
  return { ok: false, error: toNarrowFailure(narrowed) };
}

/**
 * Fetch, narrow, and handle auth.
 *
 * At most one retry: a 401 asks for a token and tries again. A second 401
 * means the stored token is wrong, so it is cleared — otherwise a bad token
 * would suppress the prompt forever and the dashboard would look permanently
 * unauthorized with no way back.
 */
export async function get<T>(opts: GetOptions<T>): Promise<ApiResult<T>> {
  const first = await attempt(opts, auth.getToken());
  if (!("retryWithAuth" in first)) return first;

  const token = await auth.requestToken();
  if (token === "") return { ok: false, error: { kind: "unauthorized" } };

  const second = await attempt(opts, token);
  if (!("retryWithAuth" in second)) return second;

  auth.clearToken();
  return { ok: false, error: { kind: "unauthorized" } };
}

/**
 * POST a control action.
 *
 * Separate from `get` rather than folded into it, because the retry semantics
 * differ in a way that matters: `get` retries a 401 after prompting for a
 * token, which is safe because reading twice is reading once. Halting twice
 * is not the same as halting once — the server is idempotent about the flag,
 * but a retry loop around a state change is a habit worth not forming. So
 * this attempts once with whatever token is already held, prompts on a 401,
 * and attempts once more. No further retries, ever.
 *
 * Also unlike `get`: a non-2xx body is still parsed, because the halt routes
 * return a JSON explanation with their 500 and that explanation is the whole
 * point ("resumed, but the halt record could not be cleared").
 */
export async function post<T>(path: string): Promise<ApiResult<T>> {
  const once = async (token: string): Promise<ApiResult<T> | { retryWithAuth: true }> => {
    let res: Response;
    try {
      res = await fetch(path, {
        method: "POST",
        headers: {
          // Required by the server on every state-changing route. Its value
          // is irrelevant; its presence is what stops the request being a
          // CORS *simple request*, which any page the operator browses could
          // otherwise fire at localhost:8080 to halt — or resume — trading.
          [CONTROL_HEADER]: "1",
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
        },
        cache: "no-store",
      });
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      return { ok: false, error: { kind: "network", message } };
    }

    if (res.status === 401) return { retryWithAuth: true };

    let body: unknown;
    try {
      body = await res.json();
    } catch {
      body = null;
    }

    if (!res.ok) {
      const explained =
        body && typeof body === "object" && "error" in body
          ? String((body as { error: unknown }).error)
          : "";
      return {
        ok: false,
        error: { kind: "http", status: res.status, message: explained },
      };
    }

    if (body === null || typeof body !== "object") {
      return { ok: false, error: { kind: "malformed", message: "expected an object" } };
    }
    return { ok: true, value: body as T };
  };

  const first = await once(auth.getToken());
  if (!("retryWithAuth" in first)) return first;

  const token = await auth.requestToken();
  if (token === "") return { ok: false, error: { kind: "unauthorized" } };

  const second = await once(token);
  if (!("retryWithAuth" in second)) return second;

  auth.clearToken();
  return { ok: false, error: { kind: "unauthorized" } };
}
