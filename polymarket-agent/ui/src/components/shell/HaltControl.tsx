import { useState } from "react";
import { halt, resume } from "../../api/endpoints";
import { failureText } from "../../api/client";
import { Badge } from "../ui/Badge";
import type { Halt } from "../../api/types";

const SOURCE_LABEL: Record<Halt["source"], string> = {
  halt_file: "HALT file",
  api: "this dashboard",
  signal: "SIGUSR1",
  circuit_breaker: "a risk limit",
  reconciliation: "a reconciliation mismatch",
};

/**
 * Stop and resume trading.
 *
 * Two things this is careful about, both because the button is the one place
 * a wrong impression costs money:
 *
 * - **Halt is not a shutdown.** The copy says so every time. An operator who
 *   believes this closed their positions will act on that belief.
 * - **Halting is confirmed, resuming is confirmed.** Neither direction is a
 *   single misplaced click, and the resume confirmation names what it is
 *   about to clear — resuming through a drawdown breaker without reading why
 *   it tripped is exactly the thing the breaker exists to slow down.
 */
export function HaltControl({
  halted,
  current,
  onChanged,
}: {
  halted: boolean;
  current: Halt | null | undefined;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);

  async function act(which: "halt" | "resume") {
    setBusy(true);
    setError(null);
    const res = which === "halt" ? await halt() : await resume();
    setBusy(false);
    setConfirming(false);
    if (!res.ok) {
      setError(failureText(res.error));
      // Still refresh: a 500 from resume means the flag moved but the record
      // did not, so the displayed state is genuinely different now.
      onChanged();
      return;
    }
    if (res.value.error) setError(res.value.error);
    onChanged();
  }

  if (!confirming) {
    return (
      <div className="halt">
        <div className="halt__state">
          {halted ? (
            <Badge tone="critical">Halted</Badge>
          ) : (
            <Badge tone="good">Trading</Badge>
          )}
          {halted && current && (
            <span className="halt__why">
              {SOURCE_LABEL[current.source] ?? current.source}
              {current.scope === "rest_of_day"
                ? " — lifts at the UTC day rollover"
                : " — stays until resumed"}
            </span>
          )}
        </div>
        {halted && current?.detail && (
          <p className="halt__detail">{current.detail}</p>
        )}
        <button
          type="button"
          className={halted ? "btn" : "btn btn--danger"}
          disabled={busy}
          onClick={() => setConfirming(true)}
        >
          {busy ? "Working…" : halted ? "Resume trading" : "Halt trading"}
        </button>
        {error && <p className="halt__error">{error}</p>}
      </div>
    );
  }

  return (
    <div className="halt">
      <p className="halt__confirm">
        {halted ? (
          <>
            Resume opening new positions?
            {current?.source === "circuit_breaker" && (
              <>
                {" "}
                A risk limit tripped this halt
                {current.detail ? ` (${current.detail})` : ""}. Resuming does
                not reset the limit — it will trip again if the condition still
                holds.
              </>
            )}
            {current?.source === "reconciliation" && (
              <>
                {" "}
                This halt means the ledger and the venue disagreed. Resuming
                accepts the venue's view; check the positions first.
              </>
            )}
          </>
        ) : (
          <>
            Stop opening new positions? Exits, order polling, reconciliation
            and settlement keep running — this does <strong>not</strong> close
            what is already open. Resting orders are cancelled on the agent's
            next wake.
          </>
        )}
      </p>
      <div className="halt__actions">
        <button
          type="button"
          className={halted ? "btn" : "btn btn--danger"}
          disabled={busy}
          onClick={() => act(halted ? "resume" : "halt")}
        >
          {busy ? "Working…" : halted ? "Yes, resume" : "Yes, halt"}
        </button>
        <button
          type="button"
          className="btn btn--quiet"
          disabled={busy}
          onClick={() => setConfirming(false)}
        >
          Cancel
        </button>
      </div>
      {error && <p className="halt__error">{error}</p>}
    </div>
  );
}
