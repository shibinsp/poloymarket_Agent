import { useSyncExternalStore } from "react";
import { auth } from "../../api/auth";
import { failureText } from "../../api/client";
import { DATASET_LABELS, type DatasetKey } from "../../api/endpoints";
import { useAllDatasets } from "../../data/useDataset";
import { dataStore } from "../../data/store";
import { useNow } from "../../data/useNow";
import { relTime } from "../../lib/format";
import { cls } from "../../lib/cls";

/**
 * One line that answers "can I trust what I am looking at?".
 *
 * It aggregates rather than shouting per endpoint: a stopped agent produces
 * seven identical network failures, and seven banners is noise. `aria-live` is
 * polite and only fires on the text changing, not on every poll.
 */
export function ConnectionBar() {
  const all = useAllDatasets();
  const now = useNow();
  const authSnap = useSyncExternalStore(auth.subscribe, auth.getSnapshot, auth.getSnapshot);

  const failing = (Object.keys(all) as DatasetKey[]).filter((k) => all[k].error);
  const cooldownLeft = authSnap.declinedUntil - now;

  if (failing.length === 0 && cooldownLeft <= 0) return null;

  const allNetwork =
    failing.length > 0 && failing.every((k) => all[k].error?.kind === "network");
  const anyAuth = failing.some((k) => all[k].error?.kind === "unauthorized");

  let tone = "warning";
  let headline: string;

  if (allNetwork) {
    tone = "critical";
    headline = "Cannot reach the agent — is it still running?";
  } else if (anyAuth) {
    headline = "A dashboard token is required to load this data.";
  } else {
    headline = `${failing.length} of ${Object.keys(all).length} endpoints returned an error.`;
  }

  return (
    <div className={cls("connbar", `connbar--${tone}`)} role="status" aria-live="polite">
      <span>{headline}</span>

      {cooldownLeft > 0 && (
        <span className="muted">
          Prompting paused for {Math.ceil(cooldownLeft / 60000)}m.
        </span>
      )}

      <span className="connbar__spacer" />

      {anyAuth && (
        <button
          className="btn btn--sm btn--primary"
          onClick={() => {
            auth.resetCooldown();
            void auth.requestToken();
          }}
        >
          Enter token
        </button>
      )}
      <button className="btn btn--sm" onClick={() => dataStore.refresh()}>
        Retry now
      </button>

      {failing.length > 0 && (
        <details>
          <summary>Details</summary>
          <ul>
            {failing.map((k) => (
              <li key={k}>
                <strong>{DATASET_LABELS[k]}</strong> — {failureText(all[k].error!)}
                {all[k].lastSuccessAt && (
                  <> (last loaded {relTime(new Date(all[k].lastSuccessAt!), now)})</>
                )}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
