/**
 * `localStorage` access that cannot throw.
 *
 * Reading or writing storage throws outright in a private window and in some
 * embedded webviews. The old dashboard wrapped this in try/catch and then
 * swallowed the failure, so settings silently failed to persist. These report
 * success so the caller can say so.
 */

export function safeGet(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function safeSet(key: string, value: string): boolean {
  try {
    window.localStorage.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}

export function safeRemove(key: string): boolean {
  try {
    window.localStorage.removeItem(key);
    return true;
  } catch {
    return false;
  }
}

/** Whether storage works at all, for a one-line warning in Settings. */
export function storageAvailable(): boolean {
  const probe = "__probe__";
  try {
    window.localStorage.setItem(probe, "1");
    window.localStorage.removeItem(probe);
    return true;
  } catch {
    return false;
  }
}
