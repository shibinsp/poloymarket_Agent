import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { auth } from "../../api/auth";

/**
 * The token prompt.
 *
 * Replaces the old `window.prompt`, which blocked the main thread, could not
 * explain itself, and looked like a phishing box. The coalescing that stops
 * seven concurrent 401s raising seven prompts lives in `api/auth.ts`; this is
 * only the surface.
 */
export function TokenDialog() {
  const snap = useSyncExternalStore(auth.subscribe, auth.getSnapshot, auth.getSnapshot);
  const [value, setValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (snap.prompting) {
      setValue("");
      inputRef.current?.focus();
    }
  }, [snap.prompting]);

  if (!snap.prompting) return null;

  return (
    <div
      className="backdrop"
      role="presentation"
      onKeyDown={(e) => {
        if (e.key === "Escape") auth.cancelPrompt();
      }}
    >
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="tok-h">
        <h2 id="tok-h">Dashboard token required</h2>
        <p>
          This agent was started with <code>DASHBOARD_TOKEN</code> set, so its
          data endpoints need that token. It is stored in this browser only.
        </p>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            auth.submitPrompt(value);
          }}
        >
          <label className="field">
            <span>Token</span>
            <input
              ref={inputRef}
              className="input"
              type="password"
              autoComplete="off"
              value={value}
              onChange={(e) => setValue(e.target.value)}
            />
          </label>
          <div className="dialog__actions">
            <button type="button" className="btn" onClick={() => auth.cancelPrompt()}>
              Not now
            </button>
            <button type="submit" className="btn btn--primary">
              Save token
            </button>
          </div>
        </form>
        <p className="muted" style={{ marginTop: 10, fontSize: 12 }}>
          Dismissing pauses the prompt for five minutes. You can set it any time
          from Settings.
        </p>
      </div>
    </div>
  );
}
