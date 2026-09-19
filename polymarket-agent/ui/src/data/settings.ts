/** Operator preferences, persisted where storage allows it. */
import { safeGet, safeSet } from "../lib/storage";

export const INTERVAL_OPTIONS = [10, 30, 60, 300, 0] as const;
export type IntervalSeconds = (typeof INTERVAL_OPTIONS)[number];
export type ThemeChoice = "system" | "light" | "dark";

export interface Settings {
  /** 0 means auto-refresh is off. The UI says so permanently when it is. */
  intervalSeconds: IntervalSeconds;
  theme: ThemeChoice;
  /** Prediction prices need 4dp; portfolio figures read better at 2. */
  pricePrecision: 2 | 4;
  /** Purely local — the agent does not serve `daily_api_budget`. */
  localDailyBudget: number | null;
  reduceMotion: boolean;
}

const KEY = "dashboard_settings";

const DEFAULTS: Settings = {
  intervalSeconds: 30,
  theme: "system",
  pricePrecision: 4,
  localDailyBudget: null,
  reduceMotion: false,
};

function load(): Settings {
  const raw = safeGet(KEY);
  if (!raw) return DEFAULTS;
  try {
    const parsed = JSON.parse(raw) as Partial<Settings>;
    return { ...DEFAULTS, ...parsed };
  } catch {
    return DEFAULTS;
  }
}

let current = load();
let lastWriteOk = true;
const listeners = new Set<() => void>();

export const settings = {
  subscribe(l: () => void): () => void {
    listeners.add(l);
    return () => listeners.delete(l);
  },
  getSnapshot(): Settings {
    return current;
  },
  /** Whether the last write reached storage — surfaced in Settings. */
  lastWriteOk(): boolean {
    return lastWriteOk;
  },
  update(patch: Partial<Settings>): void {
    current = { ...current, ...patch };
    lastWriteOk = safeSet(KEY, JSON.stringify(current));
    for (const l of listeners) l();
  },
  reset(): void {
    current = DEFAULTS;
    lastWriteOk = safeSet(KEY, JSON.stringify(current));
    for (const l of listeners) l();
  },
};

export function applyTheme(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === "system") delete root.dataset.theme;
  else root.dataset.theme = choice;
}
