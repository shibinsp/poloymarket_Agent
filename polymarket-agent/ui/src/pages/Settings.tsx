import { useState, useSyncExternalStore } from "react";
import { Card } from "../components/ui/Card";
import { Badge } from "../components/ui/Badge";
import { auth } from "../api/auth";
import { get } from "../api/client";
import { asObject } from "../api/narrow";
import { DATASET_PATHS } from "../api/endpoints";
import { failureText } from "../api/client";
import {
  INTERVAL_OPTIONS,
  applyTheme,
  settings,
  type IntervalSeconds,
  type ThemeChoice,
} from "../data/settings";
import { storageAvailable } from "../lib/storage";
import { money } from "../lib/format";

export function Settings() {
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);
  const authSnap = useSyncExternalStore(auth.subscribe, auth.getSnapshot, auth.getSnapshot);

  const [draft, setDraft] = useState("");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [budgetDraft, setBudgetDraft] = useState(
    cfg.localDailyBudget === null ? "" : String(cfg.localDailyBudget),
  );

  const canPersist = storageAvailable();

  /**
   * A one-shot authenticated request, so the operator can find out whether a
   * token works without waiting for a poll to fail. It also distinguishes the
   * case nobody expects: an agent started *without* DASHBOARD_TOKEN leaves the
   * middleware as a pass-through, so no token is needed at all.
   */
  async function testToken() {
    setTesting(true);
    setTestResult(null);
    const res = await get({ path: DATASET_PATHS.metrics, narrow: asObject });
    setTesting(false);
    if (res.ok) {
      setTestResult(
        auth.getSnapshot().hasToken
          ? "Success — the token is accepted."
          : "Success — this agent does not require a token.",
      );
    } else {
      setTestResult(failureText(res.error));
    }
  }

  return (
    <div className="page">
      {!canPersist && (
        <div className="connbar connbar--warning">
          Browser storage is unavailable (private browsing, most likely), so
          nothing on this page will be remembered after a reload.
        </div>
      )}

      <Card title="Dashboard token">
        <p className="muted" style={{ marginTop: 0 }}>
          Needed only when the agent was started with <code>DASHBOARD_TOKEN</code>{" "}
          set. Stored in this browser and sent as a bearer header.
        </p>
        <div className="filters">
          <label className="field" style={{ flex: 1, minWidth: 220 }}>
            <span>
              Token{" "}
              {authSnap.hasToken && (
                <Badge tone="good">saved · …{authSnap.hint}</Badge>
              )}
            </span>
            <input
              className="input"
              type="password"
              autoComplete="off"
              placeholder={authSnap.hasToken ? "•••• (unchanged)" : "not set"}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
            />
          </label>
          <button
            className="btn btn--primary"
            disabled={draft.trim() === ""}
            onClick={() => {
              auth.setToken(draft);
              setDraft("");
              setTestResult(null);
            }}
          >
            Save
          </button>
          <button
            className="btn"
            disabled={!authSnap.hasToken}
            onClick={() => {
              auth.clearToken();
              setTestResult(null);
            }}
          >
            Clear
          </button>
          <button className="btn" onClick={testToken} disabled={testing}>
            {testing ? "Testing…" : "Test"}
          </button>
        </div>
        {testResult && (
          <div className="state__msg" style={{ marginTop: 10 }}>{testResult}</div>
        )}
      </Card>

      <Card title="Refresh">
        <div className="filters">
          <label className="field">
            <span>Automatic refresh</span>
            <select
              className="select"
              value={cfg.intervalSeconds}
              onChange={(e) =>
                settings.update({
                  intervalSeconds: Number(e.target.value) as IntervalSeconds,
                })
              }
            >
              {INTERVAL_OPTIONS.map((s) => (
                <option key={s} value={s}>
                  {s === 0 ? "Off" : s < 60 ? `Every ${s}s` : `Every ${s / 60}m`}
                </option>
              ))}
            </select>
          </label>
        </div>
        <p className="muted" style={{ marginBottom: 0, fontSize: 12 }}>
          Polling pauses automatically while this tab is in the background, and
          resumes when you return. History endpoints are fetched less often than
          the live ones, since they are unbounded and change once per cycle.
        </p>
      </Card>

      <Card title="Display">
        <div className="filters">
          <label className="field">
            <span>Theme</span>
            <select
              className="select"
              value={cfg.theme}
              onChange={(e) => {
                const theme = e.target.value as ThemeChoice;
                settings.update({ theme });
                applyTheme(theme);
              }}
            >
              <option value="system">Match system</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </select>
          </label>
          <label className="field">
            <span>Price precision</span>
            <select
              className="select"
              value={cfg.pricePrecision}
              onChange={(e) =>
                settings.update({ pricePrecision: Number(e.target.value) as 2 | 4 })
              }
            >
              <option value={4}>4 decimals</option>
              <option value={2}>2 decimals</option>
            </select>
          </label>
        </div>
        <p className="muted" style={{ marginBottom: 0, fontSize: 12 }}>
          Prediction-market contracts trade in the third and fourth decimal, so
          rounding prices to cents loses real differences between trades.
        </p>
      </Card>

      <Card title="Local daily budget">
        <p className="muted" style={{ marginTop: 0 }}>
          The agent's own <code>daily_api_budget</code> is not exposed over the
          API. This is a display aid for the Costs page and changes nothing about
          how the agent behaves.
        </p>
        <div className="filters">
          <label className="field">
            <span>Budget (USD)</span>
            <input
              className="input"
              inputMode="decimal"
              placeholder="none"
              value={budgetDraft}
              onChange={(e) => setBudgetDraft(e.target.value)}
            />
          </label>
          <button
            className="btn"
            onClick={() => {
              const v = budgetDraft.trim();
              const n = v === "" ? null : Number(v);
              settings.update({
                localDailyBudget: n === null || !Number.isFinite(n) ? null : n,
              });
            }}
          >
            Save
          </button>
          {cfg.localDailyBudget !== null && (
            <span className="muted">Currently {money(cfg.localDailyBudget)}</span>
          )}
        </div>
      </Card>

      <Card title="Reset">
        <button
          className="btn"
          onClick={() => {
            settings.reset();
            applyTheme("system");
            setBudgetDraft("");
          }}
        >
          Clear all local settings
        </button>
        <p className="muted" style={{ marginBottom: 0, fontSize: 12, marginTop: 8 }}>
          Does not clear the saved token — use Clear above for that.
        </p>
      </Card>
    </div>
  );
}
