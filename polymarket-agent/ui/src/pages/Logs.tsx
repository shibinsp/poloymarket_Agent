import { useEffect, useRef, useState, useCallback } from "react";
import { useDataset } from "../data/useDataset";
import { Badge } from "../components/ui/Badge";
import type { Health } from "../api/types";

interface LogsResponse {
  log_file: string;
  lines: string[];
  count: number;
}

interface ParsedLogLine {
  raw: string;
  level: "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE" | "UNKNOWN";
  timestamp: string | null;
  message: string;
  fields: Record<string, string>;
}

/**
 * Parse a structured tracing log line. Handles both plain text lines and
 * JSON-structured entries emitted by tracing-subscriber's JSON formatter.
 *
 * Plain text example:
 *   2026-10-27T10:23:45.123456Z  INFO polymarket_agent: Cycle 42 started
 *
 * JSON example:
 *   {"timestamp":"2026-10-27T10:23:45.123456Z","level":"INFO","fields":{"message":"Cycle 42 started"},...}
 */
function parseLine(raw: string): ParsedLogLine {
  const trimmed = raw.trim();

  // ── JSON structured log ──────────────────────────────────────────────────
  if (trimmed.startsWith("{")) {
    try {
      const obj = JSON.parse(trimmed) as Record<string, unknown>;

      const level = String(obj.level ?? obj.severity ?? "UNKNOWN").toUpperCase() as ParsedLogLine["level"];
      const timestamp = String(obj.timestamp ?? obj.time ?? "");
      const fields = (obj.fields ?? {}) as Record<string, unknown>;
      const message = String(
        fields.message ?? fields.msg ?? obj.message ?? obj.msg ?? trimmed
      );

      // Collect extra fields (excluding well-known top-level keys)
      const extraFields: Record<string, string> = {};
      for (const [k, v] of Object.entries(fields)) {
        if (k !== "message" && k !== "msg") {
          extraFields[k] = String(v);
        }
      }
      // Also capture span/target at top level
      for (const k of ["target", "span", "thread_name"] as const) {
        if (obj[k] != null) extraFields[k] = String(obj[k]);
      }

      return { raw, level: normalizeLevel(level), timestamp: timestamp || null, message, fields: extraFields };
    } catch {
      // fall through to text parsing
    }
  }

  // ── Plain text tracing log ───────────────────────────────────────────────
  // Pattern: "2026-10-27T10:23:45.123456Z  INFO target: message  key=val"
  const textRe = /^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)\s+(TRACE|DEBUG|INFO|WARN|ERROR)\s+(.*)$/i;
  const m = trimmed.match(textRe);
  if (m) {
    const ts = m[1];
    const lvl = m[2].toUpperCase() as ParsedLogLine["level"];
    const rest = m[3];

    // Split "target: message  field=val field2=val2"
    const colonIdx = rest.indexOf(":");
    const body = colonIdx !== -1 ? rest.slice(colonIdx + 1).trim() : rest;

    // Extract trailing key=value pairs
    const fields: Record<string, string> = {};
    const kvRe = /(\w+)=("[^"]*"|\S+)/g;
    // Try to parse kvs from the end of body
    const bodyClean = body.replace(kvRe, (_, k, v) => {
      fields[k] = v.replace(/^"|"$/g, "");
      return "";
    }).trim();

    return {
      raw,
      level: normalizeLevel(lvl),
      timestamp: ts,
      message: bodyClean || body,
      fields,
    };
  }

  // ── Fallback: raw line ───────────────────────────────────────────────────
  const fallbackLevel = detectLevel(trimmed);
  return { raw, level: fallbackLevel, timestamp: null, message: trimmed, fields: {} };
}

function normalizeLevel(raw: string): ParsedLogLine["level"] {
  switch (raw.toUpperCase()) {
    case "ERROR": return "ERROR";
    case "WARN":
    case "WARNING": return "WARN";
    case "INFO": return "INFO";
    case "DEBUG": return "DEBUG";
    case "TRACE": return "TRACE";
    default: return "UNKNOWN";
  }
}

function detectLevel(line: string): ParsedLogLine["level"] {
  const up = line.toUpperCase();
  if (up.includes("ERROR")) return "ERROR";
  if (up.includes("WARN")) return "WARN";
  if (up.includes("INFO")) return "INFO";
  if (up.includes("DEBUG")) return "DEBUG";
  if (up.includes("TRACE")) return "TRACE";
  return "UNKNOWN";
}

/** Colour per log level */
const LEVEL_COLOR: Record<ParsedLogLine["level"], string> = {
  ERROR: "#f6465d",
  WARN:  "#f0b90b",
  INFO:  "#7a869e",
  DEBUG: "#4d9de0",
  TRACE: "#5e5e7a",
  UNKNOWN: "#555",
};

const LEVEL_BG: Record<ParsedLogLine["level"], string> = {
  ERROR: "rgba(246,70,93,0.08)",
  WARN:  "rgba(240,185,11,0.07)",
  INFO:  "transparent",
  DEBUG: "rgba(77,157,224,0.05)",
  TRACE: "transparent",
  UNKNOWN: "transparent",
};

function formatTimestamp(ts: string | null): string {
  if (!ts) return "";
  // Show only time portion for readability
  const idx = ts.indexOf("T");
  if (idx !== -1) {
    const time = ts.slice(idx + 1, idx + 9); // HH:MM:SS
    return `[${ts.slice(0, 10)} ${time}]`;
  }
  return `[${ts}]`;
}

function LogLine({ line }: { line: ParsedLogLine }) {
  const color = LEVEL_COLOR[line.level];
  const bg = LEVEL_BG[line.level];

  const extraFields = Object.entries(line.fields);

  return (
    <div
      style={{
        display: "flex",
        gap: "8px",
        padding: "2px 8px",
        background: bg,
        borderLeft: line.level === "ERROR" || line.level === "WARN"
          ? `3px solid ${color}`
          : "3px solid transparent",
        alignItems: "flex-start",
        fontFamily: "'JetBrains Mono', 'Fira Code', 'Consolas', monospace",
        fontSize: "11.5px",
        lineHeight: "1.6",
        wordBreak: "break-all",
      }}
    >
      {line.timestamp && (
        <span style={{ color: "#4a5568", flexShrink: 0, userSelect: "none" }}>
          {formatTimestamp(line.timestamp)}
        </span>
      )}
      <span
        style={{
          color,
          fontWeight: "700",
          flexShrink: 0,
          minWidth: "46px",
          userSelect: "none",
        }}
      >
        {line.level}
      </span>
      <span style={{ color: "#c9d1d9", flex: 1 }}>
        {line.message}
        {extraFields.length > 0 && (
          <span style={{ color: "#6b7280", marginLeft: "8px" }}>
            {extraFields.map(([k, v]) => (
              <span key={k} style={{ marginRight: "8px" }}>
                <span style={{ color: "#7c8fa8" }}>{k}</span>=
                <span style={{ color: "#a8b3c4" }}>{v}</span>
              </span>
            ))}
          </span>
        )}
      </span>
    </div>
  );
}

export function Logs() {
  const health = useDataset<Health>("health");
  const [logs, setLogs] = useState<ParsedLogLine[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [paused, setPaused] = useState(false);
  const [filterLevel, setFilterLevel] = useState<"ALL" | "INFO" | "WARN" | "ERROR">("ALL");
  const [searchText, setSearchText] = useState("");
  const [cleared, setCleared] = useState(false);
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const pausedRef = useRef(false);

  // Keep ref in sync so the polling closure sees the latest value
  useEffect(() => { pausedRef.current = paused; }, [paused]);

  const fetchLogs = useCallback(async () => {
    if (pausedRef.current) return;
    try {
      const res = await fetch("/api/logs?lines=300");
      if (!res.ok) {
        setError(`HTTP ${res.status}: ${res.statusText}`);
        return;
      }
      const data = await res.json() as LogsResponse;
      const parsed = (data.lines ?? []).map(parseLine);
      setLogs(parsed);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Initial fetch + 2s polling
  useEffect(() => {
    fetchLogs();
    const id = setInterval(fetchLogs, 2000);
    return () => clearInterval(id);
  }, [fetchLogs]);

  // Auto-scroll to bottom unless paused
  useEffect(() => {
    if (!paused && !cleared && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: "smooth" });
    }
  }, [logs, paused, cleared]);

  const visibleLogs = logs.filter((l) => {
    if (cleared) return false;
    if (filterLevel !== "ALL" && l.level !== filterLevel) return false;
    if (searchText) {
      const q = searchText.toLowerCase();
      return l.message.toLowerCase().includes(q) || l.raw.toLowerCase().includes(q);
    }
    return true;
  });

  const errorCount = logs.filter(l => l.level === "ERROR").length;
  const warnCount = logs.filter(l => l.level === "WARN").length;

  const agentStatus = health.data?.status ?? null;
  const agentHalted = health.data?.halted ?? false;
  const agentCycle = health.data?.cycle_number ?? null;

  return (
    <div className="page">
      {/* ── Agent Health Summary ── */}
      <div className="card" style={{ padding: "14px 20px", marginBottom: "4px" }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: "10px" }}>
          <div>
            <h2 style={{ margin: 0, fontSize: "17px", fontWeight: "700", color: "var(--text-primary)" }}>
              📋 Live Agent Logs
            </h2>
            <p style={{ margin: "3px 0 0", fontSize: "12px", color: "var(--text-secondary)" }}>
              Streaming from <code style={{ background: "var(--surface-2)", padding: "1px 5px", borderRadius: "3px" }}>/tmp/moneyai.log</code>
              &nbsp;· Polling every 2 seconds
            </p>
          </div>
          <div style={{ display: "flex", gap: "8px", alignItems: "center", flexWrap: "wrap" }}>
            {agentStatus && (
              <Badge tone={agentHalted ? "critical" : "good"}>
                {agentHalted ? "🔴 HALTED" : "🟢 AGENT RUNNING"}
              </Badge>
            )}
            {agentCycle != null && (
              <Badge tone="neutral">Cycle #{agentCycle}</Badge>
            )}
            {errorCount > 0 && (
              <Badge tone="critical">{errorCount} ERROR{errorCount !== 1 ? "S" : ""}</Badge>
            )}
            {warnCount > 0 && (
              <Badge tone="warning">{warnCount} WARN{warnCount !== 1 ? "S" : ""}</Badge>
            )}
          </div>
        </div>
      </div>

      {/* ── Controls ── */}
      <div style={{
        display: "flex",
        gap: "10px",
        alignItems: "center",
        flexWrap: "wrap",
        padding: "10px 0",
        marginBottom: "4px",
      }}>
        {/* Level filter */}
        <div style={{ display: "flex", gap: "4px" }}>
          {(["ALL", "INFO", "WARN", "ERROR"] as const).map(lvl => (
            <button
              key={lvl}
              type="button"
              className={`btn btn--sm ${filterLevel === lvl ? "btn--primary" : ""}`}
              style={
                filterLevel !== lvl && lvl !== "ALL"
                  ? { color: LEVEL_COLOR[lvl as ParsedLogLine["level"]] }
                  : {}
              }
              onClick={() => setFilterLevel(lvl)}
            >
              {lvl}
            </button>
          ))}
        </div>

        {/* Search */}
        <input
          type="search"
          placeholder="Search logs…"
          value={searchText}
          onChange={e => setSearchText(e.target.value)}
          style={{
            background: "var(--surface-2)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-sm)",
            color: "var(--text-primary)",
            padding: "4px 10px",
            fontSize: "12px",
            minWidth: "180px",
          }}
        />

        {/* Pause scroll */}
        <label style={{ display: "flex", alignItems: "center", gap: "6px", fontSize: "12px", color: "var(--text-secondary)", cursor: "pointer" }}>
          <input
            type="checkbox"
            checked={paused}
            onChange={e => setPaused(e.target.checked)}
          />
          Pause scroll
        </label>

        {/* Clear */}
        <button
          type="button"
          className="btn btn--sm"
          onClick={() => {
            setCleared(true);
            setTimeout(() => setCleared(false), 0); // re-show on next fetch
            setLogs([]);
          }}
        >
          🗑 Clear
        </button>

        {/* Refresh now */}
        <button type="button" className="btn btn--sm btn--primary" onClick={fetchLogs}>
          ↻ Refresh
        </button>

        <span style={{ fontSize: "11px", color: "var(--text-muted)", marginLeft: "auto" }}>
          {visibleLogs.length} line{visibleLogs.length !== 1 ? "s" : ""} shown
          {paused && <span style={{ color: "#f0b90b", marginLeft: "8px" }}>⏸ Scroll paused</span>}
        </span>
      </div>

      {/* ── Log Viewer ── */}
      <div
        ref={containerRef}
        style={{
          background: "#0d1117",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius-sm)",
          overflowY: "auto",
          maxHeight: "calc(100vh - 280px)",
          minHeight: "400px",
        }}
      >
        {loading && logs.length === 0 && (
          <div style={{ padding: "24px", textAlign: "center", color: "#7a869e", fontSize: "13px" }}>
            ⏳ Loading log file…
          </div>
        )}

        {error && (
          <div style={{
            padding: "16px",
            margin: "12px",
            background: "rgba(246,70,93,0.1)",
            border: "1px solid #f6465d",
            borderRadius: "6px",
            color: "#f6465d",
            fontSize: "12px",
            fontFamily: "monospace",
          }}>
            <strong>Error fetching logs:</strong> {error}
            <br />
            <span style={{ color: "#7a869e" }}>Make sure the agent is running and /tmp/moneyai.log exists.</span>
          </div>
        )}

        {!loading && !error && visibleLogs.length === 0 && (
          <div style={{ padding: "32px", textAlign: "center", color: "#7a869e", fontSize: "13px" }}>
            {searchText || filterLevel !== "ALL"
              ? "No log lines match the current filter."
              : "No log lines found. The agent may not have started writing to /tmp/moneyai.log yet."}
          </div>
        )}

        {visibleLogs.map((line, i) => (
          <LogLine key={i} line={line} />
        ))}

        <div ref={bottomRef} />
      </div>
    </div>
  );
}
