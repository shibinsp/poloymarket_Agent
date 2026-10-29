import { failureText } from "../../api/client";
import { DATASET_LABELS, type DatasetKey } from "../../api/endpoints";
import { useAllDatasets } from "../../data/useDataset";
import { dataStore } from "../../data/store";
import { useNow } from "../../data/useNow";
import { relTime } from "../../lib/format";
import { cls } from "../../lib/cls";

export function ConnectionBar() {
  const all = useAllDatasets();
  const now = useNow();

  const failing = (Object.keys(all) as DatasetKey[]).filter((k) => all[k].error);
  if (failing.length === 0) return null;

  const allNetwork =
    failing.length > 0 && failing.every((k) => all[k].error?.kind === "network");

  let tone = "warning";
  let headline: string;

  if (allNetwork) {
    tone = "critical";
    headline = "Cannot reach the agent — is it still running?";
  } else {
    headline = `${failing.length} of ${Object.keys(all).length} endpoints returned an error.`;
  }

  return (
    <div className={cls("connbar", `connbar--${tone}`)} role="status" aria-live="polite">
      <span>{headline}</span>
      <span className="connbar__spacer" />

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
