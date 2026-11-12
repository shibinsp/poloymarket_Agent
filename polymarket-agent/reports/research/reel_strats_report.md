# The reel's four strategies, tested

Exact rules from the reel, after costs. Holdout = the last 25% of each test's trades. Settings nudged about 20% for the neighbour gate (gate 6, walk-forward, is not used here).

| Strategy | Trades | Dates | Win % | Avg per trade | Profit factor | Total | Worst fall | Holdout | Years up | Nudged settings + | Luck score | Monte Carlo | Verdict | Gates failed |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 2. Turnaround Tuesday (DIA, Dow 30) | 416 | 2001-12-17 to 2026-11-02 | 56.2% | +0.227% | 1.52 | +94.4% | -13.9% | +18.0% (from 2020-09-28) | 16/20 | 5/5 | 0.99 | 100% | **PASS** | none |
| 3. Daily long bias (QQQ, NASDAQ) | 6289 | 2001-11-09 to 2026-11-09 | 51.4% | -0.025% | 0.94 | -159.7% | -173.8% | -31.5% (from 2020-08-06) | 6/20 | 0/3 | 0.03 | 4% | **FAIL** | 2 both halves +, 3 60% of years +, 4 profit at 2x costs, 5 nudged settings 60% +, 7 beats luck bar, 8 holdout (last 25%) +, 9 Monte Carlo 95% |
| 4. ORB NASDAQ (QQQ 5-min) | 2712 | 2016-01-04 to 2026-11-10 | 46.6% | -0.031% | 0.88 | -84.5% | -95.4% | -6.7% (from 2024-02-28) | 2/9 | 0/8 | 0.00 | 1% | **FAIL** | 2 both halves +, 3 60% of years +, 4 profit at 2x costs, 5 nudged settings 60% +, 7 beats luck bar, 8 holdout (last 25%) +, 9 Monte Carlo 95% |
| 1. Morning range breakout (USDJPY) | 711 | 2024-01-25 to 2026-11-11 | 43.0% | +0.021% | 1.18 | +15.3% | -7.1% | +2.4% (from 2026-02-27) | 2/3 | 6/6 | 0.80 | 95% | **FAIL** | 7 beats luck bar, 9 Monte Carlo 95% |

For scale: holding DIA made +9.4% a year. QQQ open-to-close (rule 3) compounded +1.9% a year; QQQ close-to-next-open (overnight) +10.9% a year.
Profit factor = money won / money lost. Totals are the sum of per-trade % returns on the position (not compounded, no leverage).
