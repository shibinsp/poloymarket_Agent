/**
 * Narrowing the API's responses into something a page can render.
 *
 * The single most important thing in this file: **every store-backed handler
 * reports failure as HTTP 200 with an object body** —
 *
 *     Ok(trades) => Json(trades),
 *     Err(e)     => Json(json!({"error": e.to_string()})),
 *
 * — so a failed `/api/trades` is a 200 whose body is `{"error": "..."}` where
 * an array belongs. `.map()` on that throws. The old dashboard survived only
 * by accident: it guarded on `data.length > 0`, and `({}).length` is
 * `undefined`, so it quietly rendered nothing and left the last good values on
 * screen. That is exactly the failure this UI exists to stop being silent.
 */
import type { ApiErrorBody } from "./types";

export function isApiError(v: unknown): v is ApiErrorBody {
  return (
    typeof v === "object" &&
    v !== null &&
    !Array.isArray(v) &&
    typeof (v as { error?: unknown }).error === "string"
  );
}

export type Narrowed<T> =
  | { ok: true; value: T }
  | { ok: false; handlerError: string }
  | { ok: false; malformed: string };

/**
 * An endpoint that returns a list. An empty array is success — "no trades yet"
 * is an empty state, not a failure.
 *
 * The error check runs *before* the array check so the server's own message
 * reaches the operator, instead of a generic "expected an array".
 */
export function asArray<T>(body: unknown): Narrowed<T[]> {
  if (isApiError(body)) return { ok: false, handlerError: body.error };
  if (Array.isArray(body)) return { ok: true, value: body as T[] };
  return {
    ok: false,
    malformed: `expected a list, got ${describe(body)}`,
  };
}

/** An endpoint that returns a single object. */
export function asObject<T>(body: unknown): Narrowed<T> {
  if (isApiError(body)) return { ok: false, handlerError: body.error };
  if (typeof body === "object" && body !== null && !Array.isArray(body)) {
    return { ok: true, value: body as T };
  }
  return { ok: false, malformed: `expected an object, got ${describe(body)}` };
}

/**
 * `/api/cycles` only. It returns a bare `null` when no cycle has run, which is
 * a successful "nothing yet" — not an error and not a malformed body.
 */
export function asObjectOrNull<T>(body: unknown): Narrowed<T | null> {
  if (body === null) return { ok: true, value: null };
  return asObject<T>(body);
}

function describe(v: unknown): string {
  if (v === null) return "null";
  if (Array.isArray(v)) return "a list";
  return typeof v;
}
