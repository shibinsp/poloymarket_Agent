# Intraday strategy search

Run: 2026-11-11 21:18 IST. Research only: nothing here trades until you say so.

## Result

- **5,596 rule variants tested** on 5-minute bars, after costs.
- **0 passed every gate.** None is safe to trade yet.
- US stocks (91): 2,824 variants, data 2026-05-01 to 2026-11-10 (89 research days + 40 holdout days).
- India NIFTY 50 stocks: 2,772 variants, data 2026-05-04 to 2026-11-09 (86 research days + 40 holdout days).

## US stocks (91)

| Gate (each applied on top of the ones before) | Pass this gate alone | Still standing |
|---|---|---|
| 1. At least 100 trades | 2,395 | 2,395 |
| 2. Both halves profitable | 266 | 177 |
| 3. 60% of months profitable | 687 | 131 |
| 4. Profitable at double costs | 382 | 105 |
| 5. 60% of its family profitable | 6 | 0 |
| 6. Family walk-forward profitable | 993 | 0 |
| 7. Beats the luck bar (deflated Sharpe) | 0 | 0 |
| 8. Holdout (last 40 days) profitable | 647 | 0 |

Luck bar: with 2,395 rules tried, the best one would show a yearly Sharpe of about **8.5** by luck alone. The rules are near-copies of each other (about 14 truly different ideas), which gives a softer bar of 4.2. The best rule found scored 4.4.

Passing every gate with the strict bar: **0**. With the softer bar: **0**.


### Closest rules (pass gates 1-6 and the holdout, ranked by Sharpe)

| Rule | Trades | Total | Holdout | Sharpe / yr | Luck score | Monte Carlo |
|---|---|---|---|---|---|---|
| none | | | | | | |

### By family

| Family | Tried | Pass gates 1-6 | Also holdout | Pass all |
|---|---|---|---|---|
| us ampm fade | 12 | 0 | 0 | 0 |
| us ampm fade +mkt | 12 | 0 | 0 | 0 |
| us ampm follow | 12 | 0 | 0 | 0 |
| us ampm follow +mkt | 12 | 0 | 0 | 0 |
| us burst30 fade | 12 | 0 | 0 | 0 |
| us burst30 fade +mkt | 12 | 0 | 0 | 0 |
| us burst30 follow | 12 | 0 | 0 | 0 |
| us burst30 follow +mkt | 12 | 0 | 0 | 0 |
| us firstmove fade | 144 | 0 | 0 | 0 |
| us firstmove fade +mkt | 144 | 0 | 0 | 0 |
| us firstmove follow | 144 | 0 | 0 | 0 |
| us firstmove follow +mkt | 144 | 0 | 0 | 0 |
| us gap fade | 48 | 0 | 0 | 0 |
| us gap fade +mkt | 48 | 0 | 0 | 0 |
| us gap follow | 48 | 0 | 0 | 0 |
| us gap follow +mkt | 48 | 0 | 0 | 0 |
| us latemove fade | 18 | 0 | 0 | 0 |
| us latemove fade +mkt | 18 | 0 | 0 | 0 |
| us latemove follow | 18 | 0 | 0 | 0 |
| us latemove follow +mkt | 18 | 0 | 0 | 0 |
| us orb both stop-far | 78 | 0 | 0 | 0 |
| us orb both stop-far +mkt | 78 | 0 | 0 | 0 |
| us orb both stop-mid | 78 | 0 | 0 | 0 |
| us orb both stop-mid +mkt | 78 | 0 | 0 | 0 |
| us orb both stop-none | 78 | 0 | 0 | 0 |
| us orb both stop-none +mkt | 78 | 0 | 0 | 0 |
| us orb long stop-far | 78 | 0 | 0 | 0 |
| us orb long stop-far +mkt | 78 | 0 | 0 | 0 |
| us orb long stop-mid | 78 | 0 | 0 | 0 |
| us orb long stop-mid +mkt | 78 | 0 | 0 | 0 |
| us orb long stop-none | 78 | 0 | 0 | 0 |
| us orb long stop-none +mkt | 78 | 0 | 0 | 0 |
| us orb short stop-far | 78 | 0 | 0 | 0 |
| us orb short stop-far +mkt | 78 | 0 | 0 | 0 |
| us orb short stop-mid | 78 | 0 | 0 | 0 |
| us orb short stop-mid +mkt | 78 | 0 | 0 | 0 |
| us orb short stop-none | 78 | 0 | 0 | 0 |
| us orb short stop-none +mkt | 78 | 0 | 0 | 0 |
| us prevday high fade stop-half-range | 3 | 0 | 0 | 0 |
| us prevday high fade stop-half-range +mkt | 3 | 0 | 0 | 0 |
| us prevday high fade stop-none | 3 | 0 | 0 | 0 |
| us prevday high fade stop-none +mkt | 3 | 0 | 0 | 0 |
| us prevday high follow stop-half-range | 3 | 0 | 0 | 0 |
| us prevday high follow stop-half-range +mkt | 3 | 0 | 0 | 0 |
| us prevday high follow stop-none | 3 | 0 | 0 | 0 |
| us prevday high follow stop-none +mkt | 3 | 0 | 0 | 0 |
| us prevday low fade stop-half-range | 3 | 0 | 0 | 0 |
| us prevday low fade stop-half-range +mkt | 3 | 0 | 0 | 0 |
| us prevday low fade stop-none | 3 | 0 | 0 | 0 |
| us prevday low fade stop-none +mkt | 3 | 0 | 0 | 0 |
| us prevday low follow stop-half-range | 3 | 0 | 0 | 0 |
| us prevday low follow stop-half-range +mkt | 3 | 0 | 0 | 0 |
| us prevday low follow stop-none | 3 | 0 | 0 | 0 |
| us prevday low follow stop-none +mkt | 3 | 0 | 0 | 0 |
| us timeofday long | 91 | 0 | 0 | 0 |
| us timeofday long +mkt | 91 | 0 | 0 | 0 |
| us timeofday short | 91 | 0 | 0 | 0 |
| us timeofday short +mkt | 91 | 0 | 0 | 0 |
| us vwap fade | 18 | 0 | 0 | 0 |
| us vwap fade +mkt | 18 | 0 | 0 | 0 |
| us vwap follow | 18 | 0 | 0 | 0 |
| us vwap follow +mkt | 18 | 0 | 0 | 0 |

### Your live US ORB rule in this test

Opening-range breakout: first 5 min is the range; both on a 5-min close beyond it within 30 min, opening volume at least 2.5x normal; stop far; exit 15 min before the close

- Trades 242, total +36.2%, holdout -3.3%, Sharpe 3.5 a year.
- Gates: 1 pass, 2 pass, 3 pass, 4 pass, 5 FAIL, 6 pass, 7 FAIL, 8 FAIL, 9 FAIL.

## India NIFTY 50 stocks

| Gate (each applied on top of the ones before) | Pass this gate alone | Still standing |
|---|---|---|
| 1. At least 100 trades | 1,772 | 1,772 |
| 2. Both halves profitable | 89 | 3 |
| 3. 60% of months profitable | 257 | 3 |
| 4. Profitable at double costs | 55 | 0 |
| 5. 60% of its family profitable | 3 | 0 |
| 6. Family walk-forward profitable | 564 | 0 |
| 7. Beats the luck bar (deflated Sharpe) | 0 | 0 |
| 8. Holdout (last 40 days) profitable | 466 | 0 |

Luck bar: with 1,772 rules tried, the best one would show a yearly Sharpe of about **19.2** by luck alone. The rules are near-copies of each other (about 15 truly different ideas), which gives a softer bar of 10.0. The best rule found scored 2.0.

Passing every gate with the strict bar: **0**. With the softer bar: **0**.


### Closest rules (pass gates 1-6 and the holdout, ranked by Sharpe)

| Rule | Trades | Total | Holdout | Sharpe / yr | Luck score | Monte Carlo |
|---|---|---|---|---|---|---|
| none | | | | | | |

### By family

| Family | Tried | Pass gates 1-6 | Also holdout | Pass all |
|---|---|---|---|---|
| in ampm fade | 12 | 0 | 0 | 0 |
| in ampm fade +mkt | 12 | 0 | 0 | 0 |
| in ampm follow | 12 | 0 | 0 | 0 |
| in ampm follow +mkt | 12 | 0 | 0 | 0 |
| in burst30 fade | 12 | 0 | 0 | 0 |
| in burst30 fade +mkt | 12 | 0 | 0 | 0 |
| in burst30 follow | 12 | 0 | 0 | 0 |
| in burst30 follow +mkt | 12 | 0 | 0 | 0 |
| in firstmove fade | 144 | 0 | 0 | 0 |
| in firstmove fade +mkt | 144 | 0 | 0 | 0 |
| in firstmove follow | 144 | 0 | 0 | 0 |
| in firstmove follow +mkt | 144 | 0 | 0 | 0 |
| in gap fade | 48 | 0 | 0 | 0 |
| in gap fade +mkt | 48 | 0 | 0 | 0 |
| in gap follow | 48 | 0 | 0 | 0 |
| in gap follow +mkt | 48 | 0 | 0 | 0 |
| in latemove fade | 18 | 0 | 0 | 0 |
| in latemove fade +mkt | 18 | 0 | 0 | 0 |
| in latemove follow | 18 | 0 | 0 | 0 |
| in latemove follow +mkt | 18 | 0 | 0 | 0 |
| in orb both stop-far | 78 | 0 | 0 | 0 |
| in orb both stop-far +mkt | 78 | 0 | 0 | 0 |
| in orb both stop-mid | 78 | 0 | 0 | 0 |
| in orb both stop-mid +mkt | 78 | 0 | 0 | 0 |
| in orb both stop-none | 78 | 0 | 0 | 0 |
| in orb both stop-none +mkt | 78 | 0 | 0 | 0 |
| in orb long stop-far | 78 | 0 | 0 | 0 |
| in orb long stop-far +mkt | 78 | 0 | 0 | 0 |
| in orb long stop-mid | 78 | 0 | 0 | 0 |
| in orb long stop-mid +mkt | 78 | 0 | 0 | 0 |
| in orb long stop-none | 78 | 0 | 0 | 0 |
| in orb long stop-none +mkt | 78 | 0 | 0 | 0 |
| in orb short stop-far | 78 | 0 | 0 | 0 |
| in orb short stop-far +mkt | 78 | 0 | 0 | 0 |
| in orb short stop-mid | 78 | 0 | 0 | 0 |
| in orb short stop-mid +mkt | 78 | 0 | 0 | 0 |
| in orb short stop-none | 78 | 0 | 0 | 0 |
| in orb short stop-none +mkt | 78 | 0 | 0 | 0 |
| in prevday high fade stop-half-range | 3 | 0 | 0 | 0 |
| in prevday high fade stop-half-range +mkt | 3 | 0 | 0 | 0 |
| in prevday high fade stop-none | 3 | 0 | 0 | 0 |
| in prevday high fade stop-none +mkt | 3 | 0 | 0 | 0 |
| in prevday high follow stop-half-range | 3 | 0 | 0 | 0 |
| in prevday high follow stop-half-range +mkt | 3 | 0 | 0 | 0 |
| in prevday high follow stop-none | 3 | 0 | 0 | 0 |
| in prevday high follow stop-none +mkt | 3 | 0 | 0 | 0 |
| in prevday low fade stop-half-range | 3 | 0 | 0 | 0 |
| in prevday low fade stop-half-range +mkt | 3 | 0 | 0 | 0 |
| in prevday low fade stop-none | 3 | 0 | 0 | 0 |
| in prevday low fade stop-none +mkt | 3 | 0 | 0 | 0 |
| in prevday low follow stop-half-range | 3 | 0 | 0 | 0 |
| in prevday low follow stop-half-range +mkt | 3 | 0 | 0 | 0 |
| in prevday low follow stop-none | 3 | 0 | 0 | 0 |
| in prevday low follow stop-none +mkt | 3 | 0 | 0 | 0 |
| in timeofday long | 78 | 0 | 0 | 0 |
| in timeofday long +mkt | 78 | 0 | 0 | 0 |
| in timeofday short | 78 | 0 | 0 | 0 |
| in timeofday short +mkt | 78 | 0 | 0 | 0 |
| in vwap fade | 18 | 0 | 0 | 0 |
| in vwap fade +mkt | 18 | 0 | 0 | 0 |
| in vwap follow | 18 | 0 | 0 | 0 |
| in vwap follow +mkt | 18 | 0 | 0 | 0 |

## How to read this

- **Per day / Total**: average return of the day's trades per rupee or dollar put in, after costs. It is not the account's return.
- **Luck score**: chance the rule is better than the best of that many random rules. It needs 0.95.
- **Holdout**: the last 40 days, which were not used to pick the rule.
- A rule that passes here still has to pass on paper before any money goes on it.

## History

2 run(s) so far. No rule has passed in any run.
