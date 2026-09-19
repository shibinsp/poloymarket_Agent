/**
 * Tests for the four places the agent's API is surprising.
 *
 * Every one of these encodes a fact confirmed against the running agent or its
 * source, and every one of them has a plausible naive implementation that is
 * silently wrong rather than visibly broken. The rest of the UI is checked by
 * looking at it; these cannot be.
 */
import { describe, expect, it } from "vitest";
import { num, sum } from "./decimal";
import { parseTs, todayUtcStart } from "./time";
import { pctFromFraction, money, moneySigned, durationMs } from "./format";
import { asArray, asObjectOrNull, isApiError } from "../api/narrow";

describe("num — Decimals arrive as strings, counts as numbers", () => {
  it("parses both JSON types", () => {
    // rust_decimal is built with serde-str, so "0.625" and 5 land in the same
    // payload and must read the same way.
    expect(num("0.625")).toBe(0.625);
    expect(num(5)).toBe(5);
    expect(num("  12.5 ")).toBe(12.5);
  });

  it("returns null rather than zero for absent or unparseable input", () => {
    // The old dashboard's `parseFloat(x || 0)` is how a missing figure became
    // a confident $0.00. "No data" and "zero" are different claims.
    expect(num(null)).toBeNull();
    expect(num(undefined)).toBeNull();
    expect(num("")).toBeNull();      // Number("") is 0 — the trap this guards
    expect(num("   ")).toBeNull();
    expect(num("n/a")).toBeNull();
    expect(num({})).toBeNull();
    expect(num(NaN)).toBeNull();
    expect(num(Infinity)).toBeNull();
  });

  it('handles the "-0" rust_decimal really emits', () => {
    expect(num("-0")).toBe(-0);
    // Rendered, it must not read as a loss that did not happen.
    expect(money(num("-0"))).toBe("$0.00");
    expect(moneySigned(num("-0"))).toBe("$0.00");
  });

  it("sums only what parses, and distinguishes empty from zero", () => {
    expect(sum(["1.5", 2, null, "oops"])).toBe(3.5);
    expect(sum([])).toBeNull();
    expect(sum([null, "x"])).toBeNull();
    expect(sum(["0"])).toBe(0);
  });
});

describe("parseTs — two timestamp formats, one of them zone-less", () => {
  it("reads a bare SQLite stamp as UTC, which is what SQLite wrote", () => {
    // datetime('now') stores UTC but writes "YYYY-MM-DD HH:MM:SS" with no
    // zone, and JS parses that as *local*. On a UTC+5:30 host that is 330
    // minutes of silent drift on every trade and cycle timestamp.
    const d = parseTs("2026-09-19 09:53:16");
    expect(d?.toISOString()).toBe("2026-09-19T09:53:16.000Z");
  });

  it("leaves an already-zoned chrono stamp alone", () => {
    const d = parseTs("2026-09-19T10:28:10.506085Z");
    expect(d?.toISOString()).toBe("2026-09-19T10:28:10.506Z");
  });

  it("agrees with a naive parse only when the host is on UTC", () => {
    const raw = "2026-09-19 09:53:16";
    const drift = new Date(raw).getTime() - parseTs(raw)!.getTime();
    // getTimezoneOffset() is minutes *behind* UTC, so it is already negative
    // east of Greenwich: on a UTC+5:30 host this asserts a -330 minute drift.
    expect(drift).toBe(new Date().getTimezoneOffset() * 60_000);
  });

  it("returns null for junk instead of an Invalid Date", () => {
    expect(parseTs(null)).toBeNull();
    expect(parseTs("")).toBeNull();
    expect(parseTs("not a date")).toBeNull();
  });

  it("uses the UTC day boundary the agent uses", () => {
    // get_today_api_cost filters on SQLite date('now'), which is UTC.
    const start = todayUtcStart(new Date("2026-09-19T02:00:00Z"));
    expect(start.toISOString()).toBe("2026-09-19T00:00:00.000Z");
  });
});

describe("pctFromFraction — every rate on the wire is a fraction", () => {
  it("scales win_rate, roi_pct and edge from fractions", () => {
    // roi_pct is a fraction despite the name: net_profit / initial_bankroll.
    expect(pctFromFraction(0.625)).toBe("62.5%");
    expect(pctFromFraction("0.095")).toBe("9.5%");
    expect(pctFromFraction(0)).toBe("0.0%");
  });

  it("renders an em dash rather than 0% when there is no figure", () => {
    expect(pctFromFraction(null)).toBe("—");
    expect(pctFromFraction(num(""))).toBe("—");
  });
});

describe("asArray — handlers report failure with HTTP 200", () => {
  it("treats a 200 carrying {error} as a failure, not as data", () => {
    // This is the shape that makes a naive .map() throw:
    //   Err(e) => Json(json!({"error": e.to_string()}))
    const body = { error: "Failed to fetch all cycles: database is locked" };
    expect(isApiError(body)).toBe(true);

    const r = asArray<never>(body);
    expect(r.ok).toBe(false);
    // The server's own words are the only diagnosis available, so they survive.
    expect(r).toHaveProperty("handlerError", body.error);
  });

  it("treats an empty list as success", () => {
    const r = asArray<number>([]);
    expect(r).toEqual({ ok: true, value: [] });
  });

  it("reports a wrong shape as malformed rather than crashing", () => {
    expect(asArray<never>({ nope: 1 }).ok).toBe(false);
    expect(asArray<never>(null).ok).toBe(false);
    expect(asArray<never>("text").ok).toBe(false);
  });

  it("checks for an error body before checking for an array", () => {
    // Ordering matters: the other way round reports "expected a list" and
    // throws away the reason the request actually failed.
    const r = asArray<never>({ error: "boom" });
    expect(r).not.toHaveProperty("malformed");
  });

  it("accepts the bare null /api/cycles returns before the first cycle", () => {
    // Successfully "nothing yet" — an empty state, not a failure.
    expect(asObjectOrNull<never>(null)).toEqual({ ok: true, value: null });
  });
});

describe("durationMs", () => {
  it("does not mix units at the origin of an axis", () => {
    expect(durationMs(0)).toBe("0s");
    expect(durationMs(450)).toBe("450ms");
    expect(durationMs(4800)).toBe("4.8s");
    expect(durationMs(199_491)).toBe("3m 19s");
    expect(durationMs(null)).toBe("—");
  });
});
