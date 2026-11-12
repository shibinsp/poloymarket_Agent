---
name: lab-notebook
description: Record and recall every trading idea we have tested, passed or failed, so the team learns from each failure and never re-tests a dead idea by accident. Use after any backtest, live review or strategy verdict, and when the user asks "did we try this before", "what have we tested", "what did we learn", "why did that fail", "show our failures", "what works so far", or "lessons".
---

# Lab notebook

File: `C:\Users\gowth\poloymarket_Agent\polymarket-agent\research\LAB_NOTEBOOK.md`.

Failures are kept on purpose. Each one is a fact about where the money is not.

## Recording a result

1. Read the notebook. If a row for the same rule already exists, update that row
   (new date, new numbers, new verdict) instead of adding a duplicate.
2. Otherwise add one row at the top of the right market's table (US, India,
   Crypto, Polymarket; add a section for a new market):
   `| YYYY-MM-DD | exact rule in one line | data, dates, costs | the numbers that decided it | VERDICT | the gate it failed or the lesson |`
3. Verdicts: PASS (all gates), WATCH (some gates, needs more data), FAIL (a gate
   it cannot pass), LIVE-FAIL (passed a backtest, failed live). Add "(live)" when
   the rule is running in the agent.
4. If the result teaches something general (a trap in our testing, a kind of
   idea that keeps failing), add or sharpen a line in "What we have learned so
   far". Keep that list short: merge, don't pile up.
5. Use real numbers from the run. Never write a verdict the gates didn't give.

## Recalling

- "Did we try X?": search the tables for X and near-copies; quote the row.
- "What works?": list the PASS and WATCH rows with their numbers.
- "What did we learn?": read out the lessons list, then the newest rows.
- If nothing matches, say so plainly and offer the `strategy-lab` skill.

Keep it in plain English. Dates are absolute (2026-11-11), never "yesterday".
