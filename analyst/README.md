# MoneyAI Analyst (restrict-only, shadow mode)

A Python sidecar that reads news, filings and market regime data. Every 15 minutes it publishes a short-lived **context** for the NSE intraday engine. It implements the multi-agent research design, constrained by the architecture doc's rule (DEV_PLAN.md §1, §7): **the analyst can only restrict trading. It never places orders, never picks direction and never scales size up.**

| Agent (multi-agent design) | Here | What it may publish |
|---|---|---|
| 1. Quant / forecasting | `forecast.py` (EWMA volatility bands), `regime.py` (India VIX, Nifty trend, FII/DII flows) | a per-stock size cut when forecast volatility is high; a market `bias` (neutral / cautious / risk_off → size × 1 / 0.75 / 0.5) |
| 2. Filings RAG | `sources/filings.py` (BSE announcements, events calendar), `rag.py` (BM25 over ingested filings and transcripts) | exclusions on results days and corporate-action days; background passages for the classifier |
| 3. News sentiment | `sources/news.py` (RSS), `classify.py` (LLM or keyword baseline; fixed labels, severity 0-3, sentiment −1..1) | exclusions for severity ≥ 2 stock-specific events; a watchlist |
| 4. Portfolio / risk | `compose.py` and `validate` (deterministic code) | the context below. **The real risk gate stays in the Rust engine (Phase 2).** |

**Direction forecasting is research only** (`forecast-eval`). It is never published. On 10 years of daily data for RELIANCE, TCS, HDFCBANK, INFY, ITC and SBIN, a walk-forward boosted-tree model on RSI/MACD/ATR/volume features had **no out-of-sample edge**: 0.1 to 3.1 points *worse* than a majority-class guess on five of them, and 0.2 points better on ITC, which is noise. This is consistent with the doc's position that the model must not pick direction.

## Output: `state/context.json`

```json
{
  "mode": "shadow",
  "run_id": "20261112T112659-531c21",
  "published_at": "2026-11-12T11:26:59+05:30",
  "expires_at":   "2026-11-12T11:46:59+05:30",
  "watchlist": ["BANKBARODA", "CANBK"],
  "bias": "cautious",
  "exclude": ["TATAPOWER"],
  "size_multiplier": 0.75,
  "symbol_size_multipliers": {"CUMMINSIND": 0.5},
  "reasons": [{"text": "Exclude TATAPOWER: ...Q2 results...", "source_ids": ["news:03381cbdeb32f42f"]}],
  "schema_version": "1.0", "prompt_version": "classify-v1", "model": "keyword-v1"
}
```

**Guarantees, enforced in code and covered by mutation-checked tests:**
- Every multiplier is within [0, 1]; a value above 1 fails validation.
- `mode` can only be `"shadow"`. Enforcement requires a code change, made only after the shadow report justifies it.
- Every reason cites a source id that exists in the run. Every symbol is in the Nifty 100 universe.
- A model answer is rejected, never repaired, if it:
  - fails the schema;
  - is about the wrong item;
  - cites evidence that was not supplied;
  - names a company that is not tagged on the item.
- Any run-level failure is a **fallback**: nothing new is published. The run is recorded, the context expires after 20 minutes, and the engine runs on its default rules.

**Consumer rule (for the Rust engine, later):**
- Read the file and validate it.
- If it is invalid, expired or `mode == "shadow"`, apply nothing.
- If enforced, it may only:
  - skip `exclude` symbols;
  - multiply size by `size_multiplier × symbol_size_multipliers[sym]`;
  - tighten entry rules on `cautious` / `risk_off`.

`store.load_active_context` is the reference reader.

## Setup

```bash
cd analyst
uv venv -p 3.12 .venv
uv pip install -p .venv -e '.[dev,data,ml]'   # data = yfinance, ml = lightgbm + scikit-learn
cp config.example.toml config.local.toml      # gitignored; edit sources and LLM here
```

- `provider = "none"` uses the deterministic keyword classifier: no model and no key.
- For an LLM, set `provider = "openai_compatible"` (NVIDIA NIM, Mistral, vLLM, Ollama) or `"anthropic"`. Put the key in `LLM_API_KEY`; `--env-file ../polymarket-agent/.env` reuses the engine's.
- **NIM note (12 Nov 2026):** `meta/llama-3.3-70b-instruct` and `meta/llama-3.2-11b-vision-instruct` return 410 Gone, and the Gemma and Mistral models listed return 404 for this key. `nvidia/nemotron-3.5-lightning-30b-a3b` works, with `extra_body = { chat_template_kwargs = { enable_thinking = false } }`.

## Run

```bash
.venv/bin/moneyai-analyst --config config.local.toml run-once            # one run; prints the context
.venv/bin/moneyai-analyst --config config.local.toml run-once --no-publish
.venv/bin/moneyai-analyst --config config.local.toml run-once --input replay.jsonl   # replay recorded items
.venv/bin/moneyai-analyst --config config.local.toml serve               # 08:45 IST, then every 15 min to 15:30, weekdays
.venv/bin/moneyai-analyst --config config.local.toml ingest q2.txt --symbol TCS --title "TCS Q2 FY27 concall" --date 2026-10-10
.venv/bin/moneyai-analyst --config config.local.toml shadow-report --engine-db ../polymarket-agent/polymarket-agent.db
.venv/bin/moneyai-analyst forecast-eval RELIANCE TCS INFY                # research only
```

Every run is recorded in `state/analyst.db`, in these tables:
- `runs`: status, errors, model, prompt and schema versions, the context;
- `classifications`: one per item, with source title and URL;
- `llm_cache`;
- `documents`: the RAG corpus.

**Source notes:**
- The BSE announcements API answered **403** to automated requests on 12 Nov 2026, so it is off by default. Use `events_calendar` (CSV: `symbol,date,event_type,description`) for results days and ex-dates.
- NSE FII/DII is opt-in; check NSE's website terms first.
- Yahoo daily bars: during market hours, today's still-forming bar is dropped.

## Test

```bash
.venv/bin/pytest          # 86 tests, offline, no keys needed
```

These tests also run in CI as the `analyst (python)` job. Before merging a change to any guard, do a **mutation check**: delete or flip the guard, and confirm a test fails. All 28 guards in this package were checked this way when it was written.

## Tune (DEV_PLAN.md §5)

1. **Shadow first.** Run `serve` alongside paper trading. Trading ignores the context.
2. **Golden set.** Hand-label 200-300 real Nifty 100 headlines, **dated after the model's training cutoff**, as JSONL (format in `golden.py`). `tests/fixtures/golden_smoke.jsonl` is a 12-item smoke file, not a golden set.
   ```bash
   moneyai-analyst --config config.local.toml golden-eval golden/labelled.jsonl --split tune --repeat
   moneyai-analyst --config config.local.toml golden-eval golden/labelled.jsonl --split tune --keyword   # baseline
   ```
   Use the 75% `tune` split while iterating. Score the 25% `holdout` split **once** at the end.
3. **Targets:**

   | Measure | Target |
   |---|---|
   | Schema-valid output | ≥ 99% |
   | Event-type accuracy | ≥ 90% |
   | Repeat stability | ≥ 95% |
   | Run time, p95 | ≤ 30 s |
   | Model must beat the keyword baseline | yes |

   **On the smoke file:**

   | Classifier | Event-type accuracy | Repeat stability |
   |---|---|---|
   | Keyword | 92% | — |
   | Nemotron | 83% | 83% |

   So the model has not yet earned its place. Stability below 95% at temperature 0 is why answers are cached.
4. **One change at a time:** a prompt, a few-shot example, a model or a threshold. Re-run golden-eval and keep the change only if the targets hold.
5. **Switch on only on evidence:** `shadow-report` must show a positive value of restrictions over 40+ trading days.
