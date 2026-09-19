import { useEffect, useSyncExternalStore } from "react";
import type { DatasetKey } from "../api/endpoints";
import { dataStore, type DatasetState } from "./store";
import { settings } from "./settings";

export interface Dataset<T> {
  data: T | undefined;
  error: DatasetState["error"];
  loading: boolean;
  lastSuccessAt: number | null;
  latencyMs: number | null;
  /** True once the data is older than 1.5x the polling interval. */
  isStale: (now: number) => boolean;
  /** True past 4x — old enough that it should be read as suspect. */
  isVeryStale: (now: number) => boolean;
}

function subscribe(l: () => void): () => void {
  return dataStore.subscribe(l);
}

/**
 * Subscribe to one dataset for the lifetime of the component.
 *
 * Staleness is measured against the *current* interval rather than an absolute
 * age, so choosing a five-minute refresh does not paint the whole page red.
 */
export function useDataset<T>(key: DatasetKey): Dataset<T> {
  useEffect(() => dataStore.retain(key), [key]);

  const all = useSyncExternalStore(
    subscribe,
    dataStore.getSnapshot,
    dataStore.getSnapshot,
  );
  const s = all[key];
  const intervalMs = settings.getSnapshot().intervalSeconds * 1000;

  const age = (now: number): number | null =>
    s.lastSuccessAt === null ? null : now - s.lastSuccessAt;

  return {
    data: s.data as T | undefined,
    error: s.error,
    loading: s.loading,
    lastSuccessAt: s.lastSuccessAt,
    latencyMs: s.latencyMs,
    isStale: (now) => {
      if (intervalMs === 0) return false;
      const a = age(now);
      return a !== null && a > intervalMs * 1.5;
    },
    isVeryStale: (now) => {
      if (intervalMs === 0) return false;
      const a = age(now);
      return a !== null && a > intervalMs * 4;
    },
  };
}

/** The whole store, for the connection bar and the endpoint matrix. */
export function useAllDatasets(): Record<DatasetKey, DatasetState> {
  return useSyncExternalStore(
    subscribe,
    dataStore.getSnapshot,
    dataStore.getSnapshot,
  );
}
