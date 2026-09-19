/**
 * The dashboard token, and the prompting policy around it.
 *
 * This is a module-level singleton rather than React state for two reasons:
 * the `get()` calls that hit a 401 happen outside the component tree, and up
 * to seven of them land in the same refresh tick. It also has to survive
 * StrictMode's deliberate double-invocation in development.
 *
 * Two behaviours are carried over from the old dashboard because they were
 * right:
 *
 *   - **Coalescing.** Seven concurrent 401s must raise *one* prompt, not
 *     seven. Callers share a single in-flight promise.
 *   - **A cooldown.** If the operator dismisses the prompt, stop asking for
 *     five minutes. Re-prompting on every 30s tick is how a dashboard becomes
 *     something people close.
 *
 * What changed: the prompt is a real dialog rather than `window.prompt`, and
 * the cooldown is *visible* in the connection bar instead of being a silent
 * interval during which the page simply looks broken.
 *
 * On storage: the token lives in `localStorage`, which any script on this
 * origin can read. That is safe here only because this app never uses
 * `dangerouslySetInnerHTML` — remote strings like `market_question` are
 * escaped by React. Keep it that way.
 */
import { safeGet, safeRemove, safeSet } from "../lib/storage";

const KEY = "dashboard_token";
const DECLINE_COOLDOWN_MS = 5 * 60 * 1000;

let token: string = safeGet(KEY) ?? "";
let declinedUntil = 0;
let promptInFlight: Promise<string> | null = null;
let resolvePrompt: ((value: string) => void) | null = null;

const listeners = new Set<() => void>();

/** Snapshot object, replaced on every change so `useSyncExternalStore` sees it. */
let snapshot = computeSnapshot();

export interface AuthSnapshot {
  hasToken: boolean;
  /** Last four characters, for display. Never the whole token. */
  hint: string;
  /** True while the dialog should be open. */
  prompting: boolean;
  /** Epoch ms until which prompting is suppressed, or 0. */
  declinedUntil: number;
}

function computeSnapshot(): AuthSnapshot {
  return {
    hasToken: token !== "",
    hint: token ? token.slice(-4) : "",
    prompting: promptInFlight !== null,
    declinedUntil,
  };
}

function notify(): void {
  snapshot = computeSnapshot();
  for (const l of listeners) l();
}

export const auth = {
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },

  getSnapshot(): AuthSnapshot {
    return snapshot;
  },

  getToken(): string {
    return token;
  },

  setToken(next: string): boolean {
    token = next.trim();
    // Saving a token is an explicit act of engagement: clear any cooldown so
    // the UI stops telling the operator it has stopped asking.
    declinedUntil = 0;
    const stored = token === "" ? safeRemove(KEY) : safeSet(KEY, token);
    notify();
    return stored;
  },

  clearToken(): void {
    auth.setToken("");
  },

  /**
   * Ask for a token, coalescing concurrent callers.
   *
   * Resolves with the token, or `""` if the operator declined or a cooldown is
   * in force — callers treat `""` as "give up for now", not as a retry signal.
   */
  requestToken(): Promise<string> {
    if (token !== "") return Promise.resolve(token);
    if (Date.now() < declinedUntil) return Promise.resolve("");
    if (promptInFlight) return promptInFlight;

    promptInFlight = new Promise<string>((resolve) => {
      resolvePrompt = resolve;
    });
    notify();
    return promptInFlight;
  },

  /** Called by the dialog when the operator saves a token. */
  submitPrompt(value: string): void {
    const trimmed = value.trim();
    auth.setToken(trimmed);
    const resolve = resolvePrompt;
    promptInFlight = null;
    resolvePrompt = null;
    notify();
    resolve?.(trimmed);
  },

  /** Called by the dialog when the operator dismisses it. */
  cancelPrompt(): void {
    declinedUntil = Date.now() + DECLINE_COOLDOWN_MS;
    const resolve = resolvePrompt;
    promptInFlight = null;
    resolvePrompt = null;
    notify();
    resolve?.("");
  },

  /** Let the operator re-open the prompt during a cooldown. */
  resetCooldown(): void {
    declinedUntil = 0;
    notify();
  },
};

export { DECLINE_COOLDOWN_MS };
