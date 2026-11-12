---
name: strategy-lab
description: Test a trading idea the honest way, from one sentence to a verdict. Turns the idea into an exact rule, checks the lab notebook so we never re-test a known failure, adds it to the intraday search pipeline (or a daily script), runs the 9 gates (100 trades, both halves, 60% of months, double costs, family, walk-forward, luck bar, holdout, Monte Carlo) and records the verdict. Use when the user says "test this strategy", "try this idea", "will this work", "find a new strategy", "research", "run the pipeline", "weekly research", "strategy search", or sends a strategy from a screenshot or video.
---

# Strategy lab

The job is to find out whether an idea makes money **after costs, on data it was
not fitted to**. Most ideas fail. A clear failure is a good result: write it down
and move on. Never soften a gate to make an idea pass.

Project: `C:\Users\gowth\poloymarket_Agent\polymarket-agent`. Paper trading only.

## Hard rules

- Read prices only. Never place, change or cancel orders from this skill.
- Never change `config/*.toml` or agent code to switch a strategy on. A PASS is
  brought to the user with its numbers; the user decides.
- Never restart the agent during market hours (India 09:15-15:30 IST, US
  20:00-02:30 IST in winter, 19:00-01:30 in summer).
- Never print keys or tokens. The pipeline reads `.env` itself.
- No personalised investment advice: report what the tests show.

## Steps

1. **Check the notebook first.** Read `research/LAB_NOTEBOOK.md`. If the idea (or
   a near-copy) was already tested, show that entry and ask whether anything is
   different this time (new data, new market, a changed rule). Do not re-run it
   unchanged.

2. **Write the exact rule** in one line the user can check: market, entry
   trigger, side, stop, exit time, costs. Example: "US, 91 stocks: if the first
   5-minute bar closes 1% or more above the open on 2.5x normal volume, buy at
   its close, stop at its low, sell 15 minutes before the close; 5 bp a side."
   If any part is vague, pick the plainest version and say so.

3. **Decide where it runs.**
   - Intraday (enter and exit the same day) on US stocks or the NIFTY 50: add a
     family to `research/pipeline/intraday_search.py` (see "Adding a family").
   - Daily or longer: write a scratchpad script on daily bars with the same
     gates (the earlier `mass_test.py` is the model).
   - Options, Polymarket, crypto: own script; reuse the gate logic. Price
     options from real historical quotes where they exist; say plainly when a
     model price (Black-Scholes) is used, because those results are weaker.

4. **Run it.** Pipeline: `python -I research/pipeline/intraday_search.py all`
   (or `run` to skip the data update). It takes a few minutes and writes
   `reports/research/intraday_report.md` and `intraday_latest.json`.

5. **Read the verdict from the gates, not from the total.** In order:
   1. at least 100 trades
   2. both halves profitable
   3. at least 60% of months profitable
   4. profitable at double costs
   5. at least 60% of the family's nearby settings profitable
   6. family walk-forward profitable (each month uses the setting that was best
      before it)
   7. deflated Sharpe at least 0.95 (the luck bar for the number of rules tried)
   8. holdout (last 40 days, never used to choose) profitable
   9. Monte Carlo: 95% of 5,000 reshuffled histories end in profit

   Report the strict luck bar and the softer one (effective number of distinct
   ideas). A rule that passes only the softer bar is a WATCH, not a PASS.

6. **Look for the traps** before calling anything a PASS (every one, every time):
   - Is the profit from a handful of trades? Re-check without the best 3 and
     the best 10 trades (Turnaround Tuesday kept +69% of +94% without its best 3).
   - **Remove market risk:** regress the daily result on the market (or the
     equal-weight basket) and report the beta-adjusted return and its t-stat,
     in research and holdout. QuantRank's "+9%/yr" was 0% after this.
   - **Remove the top 3 contributing stocks** in the holdout (QuantRank's
     holdout gain was MU, SHOP and AMD).
   - **Leak check on price-level formulas:** our 25-year prices are adjusted
     backwards, so anything using a price in dollars can see the future.
     Recompute after a fake split per stock (price / k, volume x k); if the
     picks move, it's a leak (gtja191_164 "made" +25%/yr this way).
   - **Placebo test:** run the same rule where it should not work (other
     weekdays, other hours, shuffled dates). If it works there too, the
     claimed reason is wrong. (The Turnaround rule loses on Wed/Thu: good.)
   - If the idea comes from a post or video, compare our numbers with theirs
     (win %, profit factor, drawdown). A big gap means different rules, data
     or costs; find out which before trusting either.
   - Is the test population the same as what the bot will trade live (same
     stocks, same data feed, same time of entry)? This is how the Polymarket
     underdog book and the IEX/SIP ORB picks fooled us.
   - Does the rule use anything not known at the moment of the trade (final
     volume, the close of the bar it enters on, a later news story)?
   - Would the live fill be worse than the test's (wide spreads, small stocks,
     the 9:30 bar)?

7. **Record it** with the `lab-notebook` skill: one row, verdict, why.

8. **Tell the user** in plain words: the rule, how many trades, the result per
   trade and per month, which gate it failed or that it passed, and what happens
   next (PASS: paper-test proposal; WATCH: re-test when more data arrives; FAIL:
   recorded, nothing changes).

## Adding a family to the pipeline

In `research/pipeline/intraday_search.py`, `Market.families()`, add a block that
builds entry bars and sides, then calls `self.add(family, key, plain_description, self.sim(entry, side, stop, exit))`:

- `entry`: array [stocks, days] of the 5-minute bar index to enter at its close,
  `-1` for no trade. `self.first(mask, lo, hi)` finds the first bar where `mask`
  is true between bars `lo` and `hi`.
- `side`: +1 long, -1 short (scalar or array).
- `stop`: price array or `None`. A stop is filled at the stop or at a worse open.
- `exit`: bar index (default 15 minutes before the close).
- Available arrays: `O H L C V` [stocks, days, bars], `Od` (day open), `PC`
  (previous close), `PDH PDL` (previous day high/low), `ADR` (14-day average
  range as a fraction), `RV1` (first-bar relative volume), `VW` (VWAP), `Rm`
  (whole-market move since the open).
- Each family should be a small grid (3-4 values per setting), not one tuned
  value; gate 5 needs neighbours to judge robustness.
- Write the description in plain words; it appears in the report.

Run `python -I research/pipeline/intraday_search.py run` and check the family
shows up in the "By family" table.

## Data

- `research/data/bars5_us.npz`, `bars5_in.npz`: 5-minute bars, built from the
  1-minute cache and extended by `update` (Alpaca SIP for the US, about 30
  minutes late; Yahoo for India, last 60 days only).
- The India data is short (from May 2026). Say so when a result rests on it.
