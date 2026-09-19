# Dashboard UI

The web dashboard served at `http://127.0.0.1:8080`. React 19 + TypeScript,
built by Vite into a **single self-contained** `../static/index.html`.

## Commands

```bash
npm install
npm run dev          # Vite on :5173, proxying /api to a running agent on :8080
npm run build        # tsc + vite → ../static/index.html
npm run build:embed  # the above, then `cargo build` so the binary picks it up
npm test             # the quirk tests (see below)
npm run typecheck
```

## The one thing to remember

`src/monitoring/dashboard.rs` serves the page with

```rust
let html = include_str!("../../static/index.html");
```

That is **compile-time**. A rebuilt bundle does not reach the binary until
Rust recompiles, and never reaches an already-running agent at all. So:

```
npm run build   →   cargo build   →   restart the agent
```

Skipping step two or three is the reason a UI change appears not to have
worked. `npm run build:embed` does the first two together, and the footer
carries a build stamp so you can tell from the page which bundle you are
looking at.

## Why it is built this way

**One inlined file** because the server has exactly one route — `/` — and no
static-asset handler. Any emitted `assets/*.js` would 404. It also keeps the
agent a single binary with nothing to deploy alongside it.

**Hash routing** (`#/trades`) for the same reason: a real path would 404 on
reload.

**Hand-rolled SVG charts**, no charting library. The previous dashboard pulled
Chart.js from a CDN, so an air-gapped VPS — a plausible deploy target — got no
chart at all. It also keeps the inlined bundle small.

## The API is surprising in four ways

All four are covered by `src/lib/quirks.test.ts`. They are the only things
here worth unit-testing; the rest is checked by looking at it.

1. **Handlers report failure with HTTP 200** and a body of
   `{"error": "..."}` where an array belongs, so a naive `.map()` throws.
   Everything is narrowed once in `api/narrow.ts` and surfaces as
   `kind: "handler"` carrying the server's own message.
2. **Decimals are strings.** `rust_decimal` is built with `serde-str`, so
   `win_rate` is `"0.625"` while `wins` is `5`. Read every number through
   `num()`, which returns `null` rather than `0` for anything unparseable.
3. **Two timestamp formats, one without a zone.** `/api/health` is RFC3339;
   trades, cycles and costs come from SQLite `datetime('now')`, which writes
   UTC as `2026-09-19 09:53:16` — and JS parses that as *local*. Use
   `parseTs()`. On a UTC+5:30 host the naive parse is 5½ hours out, silently.
4. **Rates are fractions.** `win_rate`, `roi_pct` (despite the name),
   `edge_at_entry`, `confidence` and `kelly_*` are all fractions.
   `pctFromFraction()` is the only percent helper, deliberately, so misuse
   reads wrong at the call site.

Also: `/api/cycles` returns a bare `null` before the first cycle (success, not
an error); `cycle_number` starts at **0**, so never test it for truthiness; and
the two trade endpoints sort in opposite directions.

## What the UI cannot show

The `trades` table carries `venue_id`, `symbol`, `quantity`, `avg_fill_price`,
`mark_price`, `stop_price` and more, but the server's `TradeRecord` declares
only the 16 legacy fields and sqlx drops the rest. Adding columns to
`api/types.ts` will not make them appear — that needs a change in
`src/db/store.rs`.
