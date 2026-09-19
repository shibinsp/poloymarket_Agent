import type { ReactNode } from "react";
import { Card } from "./Card";
import { ErrorState, Skeleton } from "./States";
import { Freshness } from "./Freshness";
import type { Dataset } from "../../data/useDataset";
import type { DatasetKey } from "../../api/endpoints";
import { dataStore } from "../../data/store";
import { useNow } from "../../data/useNow";

/**
 * A card wired to one dataset, so every page handles loading, failure and
 * staleness the same way.
 *
 * The rule that matters is in the failure branch: if the fetch failed but we
 * still hold the last good value, the content is *dimmed and labelled stale*
 * rather than replaced. Those numbers remain the most useful thing on screen —
 * what was unacceptable before was showing them as though they were current.
 */
export function DatasetCard<T>({
  title,
  label,
  dataset,
  datasetKey,
  actions,
  children,
}: {
  title: ReactNode;
  /** Used in the failure message, e.g. "All cycles could not be loaded". */
  label: string;
  dataset: Dataset<T>;
  datasetKey: DatasetKey;
  actions?: ReactNode;
  children: (data: T) => ReactNode;
}) {
  const now = useNow();
  const hasData = dataset.data !== undefined;

  return (
    <Card
      title={title}
      actions={actions}
      dimmed={Boolean(dataset.error) && hasData}
      footer={
        <Freshness
          lastSuccessAt={dataset.lastSuccessAt}
          now={now}
          stale={dataset.isStale(now)}
          veryStale={dataset.isVeryStale(now)}
          error={dataset.error}
          paused={dataStore.isPaused()}
        />
      }
    >
      {!hasData && dataset.error ? (
        <ErrorState
          label={label}
          error={dataset.error}
          onRetry={() => dataStore.refresh(datasetKey)}
        />
      ) : !hasData ? (
        <Skeleton />
      ) : (
        children(dataset.data as T)
      )}
    </Card>
  );
}
