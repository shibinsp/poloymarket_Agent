import { useDataset, useAllDatasets } from "../data/useDataset";
import { DatasetCard } from "../components/ui/DatasetCard";
import { Card } from "../components/ui/Card";
import { StatTile } from "../components/ui/StatTile";
import { Badge, agentStateTone, type StatusTone } from "../components/ui/Badge";
import { DataTable } from "../components/ui/DataTable";
import { failureText } from "../api/client";
import { DATASET_KEYS, DATASET_LABELS, DATASET_PATHS, type DatasetKey } from "../api/endpoints";
import { useNow } from "../data/useNow";
import { absTime, durationSec, int, relTime, DASH } from "../lib/format";
import { parseTs } from "../lib/time";
import type { Health as HealthPayload } from "../api/types";

export function Health() {
  const health = useDataset<HealthPayload>("health");
  const all = useAllDatasets();
  const now = useNow();

  return (
    <div className="page">
      <DatasetCard title="Agent" label="Health" dataset={health} datasetKey="health">
        {(h) => {
          const lastCycle = parseTs(h.last_cycle_at);
          const started = parseTs(h.started_at);

          /*
           * These two fields only exist on builds that include the cycle
           * watchdog. Test for the *key*, not the value: on a new build they
           * are legitimately null before the first cycle has been scheduled,
           * so a truthiness check would wrongly report an old server.
           */
          const hasDue = "next_cycle_due" in h;
          const hasAlerts = "alerts_delivering" in h;

          const due = hasDue ? parseTs(h.next_cycle_due ?? null) : null;
          const overdueMs = due ? now - due.getTime() : null;

          return (
            <>
              <div className="grid grid--tiles">
                <StatTile
                  label="Status"
                  value={<Badge tone={statusTone(h.status)}>{h.status}</Badge>}
                />
                <StatTile
                  label="Lifecycle state"
                  value={<Badge tone={agentStateTone(h.agent_state)}>{h.agent_state}</Badge>}
                />
                <StatTile label="Cycle number" value={int(h.cycle_number)} />
                <StatTile
                  label="Uptime"
                  /* The server computes this from its own clock, so it is
                     immune to skew between the agent host and this browser. */
                  value={durationSec(h.uptime_seconds)}
                  sub={started ? `started ${relTime(started, now)}` : undefined}
                  title={started ? absTime(started) : undefined}
                />
                <StatTile
                  label="Last completed cycle"
                  value={lastCycle ? relTime(lastCycle, now) : "never"}
                  sub={lastCycle ? undefined : "no cycle has finished yet"}
                  title={lastCycle ? absTime(lastCycle) : undefined}
                />
                {hasDue && (
                  <StatTile
                    label="Next cycle due"
                    value={
                      due === null
                        ? "not scheduled"
                        : relTime(due, now)
                    }
                    sub={
                      due === null
                        ? "set once a cycle is scheduled"
                        : overdueMs !== null && overdueMs > 0
                          ? "overdue"
                          : undefined
                    }
                    tone={overdueMs !== null && overdueMs > 0 ? "neg" : null}
                    title={due ? absTime(due) : undefined}
                  />
                )}
              </div>

              {hasAlerts && h.alerts_delivering === false && (
                <div style={{ marginTop: 12 }}>
                  {/* Icon plus label, never colour alone. */}
                  <Badge tone="serious" title="The agent could not deliver its last alert">
                    ⚠ Alert delivery is failing
                  </Badge>
                  <p className="muted" style={{ fontSize: 12, marginBottom: 0 }}>
                    The agent is running but its alerts are not getting out, so
                    silence from it does not mean all is well.
                  </p>
                </div>
              )}

              {!hasDue && (
                <p className="muted" style={{ fontSize: 12, marginTop: 12, marginBottom: 0 }}>
                  This agent build does not report a next-cycle schedule or alert
                  delivery state.
                </p>
              )}
            </>
          );
        }}
      </DatasetCard>

      <Card title="Endpoints">
        {/*
          Measured by this browser, not reported by the agent. It turns "the
          dashboard looks fine" into "all-cycles has been failing for fourteen
          minutes with 'database is locked'".
        */}
        <p className="muted" style={{ marginTop: 0 }}>
          What this browser has seen from each route. Endpoints nothing is
          subscribed to are simply never called.
        </p>
        <DataTable
          rows={DATASET_KEYS.map((k) => ({ key: k, ...all[k] }))}
          rowKey={(r) => r.key}
          columns={[
            { key: "n", header: "Dataset", render: (r) => DATASET_LABELS[r.key as DatasetKey] },
            { key: "p", header: "Path", render: (r) => <code className="mono">{DATASET_PATHS[r.key as DatasetKey]}</code> },
            {
              key: "s",
              header: "Last result",
              render: (r) =>
                r.error ? (
                  <Badge tone="critical">error</Badge>
                ) : r.lastSuccessAt ? (
                  <Badge tone="good">ok</Badge>
                ) : (
                  <span className="muted">not called</span>
                ),
            },
            {
              key: "w",
              header: "Last success",
              render: (r) =>
                r.lastSuccessAt ? relTime(new Date(r.lastSuccessAt), now) : DASH,
              title: (r) => (r.lastSuccessAt ? absTime(new Date(r.lastSuccessAt)) : undefined),
            },
            { key: "l", header: "Latency", numeric: true, render: (r) => (r.latencyMs === null ? DASH : `${r.latencyMs} ms`) },
            {
              key: "e",
              header: "Detail",
              truncate: true,
              render: (r) => (r.error ? failureText(r.error) : DASH),
              title: (r) => (r.error ? failureText(r.error) : undefined),
            },
          ]}
        />
      </Card>

      <DatasetCard title="Raw payload" label="Health" dataset={health} datasetKey="health">
        {(h) => (
          <pre className="state__msg" style={{ textAlign: "left" }}>
            {JSON.stringify(h, null, 2)}
          </pre>
        )}
      </DatasetCard>
    </div>
  );
}

function statusTone(status: string): StatusTone {
  switch (status.toLowerCase()) {
    case "ok":
      return "good";
    case "degraded":
      return "warning";
    case "dead":
    case "error":
      return "critical";
    default:
      return "neutral";
  }
}
