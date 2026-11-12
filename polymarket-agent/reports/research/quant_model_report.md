# QuantRank, Kalman pairs and baselines

Run: 2026-11-11 21:34 IST. Research only: nothing here trades until you say so.

## US stocks (91)

90 stocks, 2001-11-12 to 2026-11-11. Research until 9 Nov 2024, holdout after. 74 variants.

| Gate (on top of the ones before) | Pass alone | Still standing |
|---|---|---|
| 1. At least 100 trades | 74 | 74 |
| 2. Both halves profitable | 19 | 19 |
| 3. 60% of years profitable | 12 | 9 |
| 4. Profitable at double costs | 24 | 9 |
| 5. 60% of its neighbours profitable | 28 | 9 |
| 6. Walk-forward profitable | 54 | 9 |
| 7. Beats the luck bar | 0 | 0 |
| 8. Holdout (since 10 Nov 2024) profitable | 32 | 0 |
| 9. Monte Carlo 95% | 4 | 0 |

Luck bar: best of 74 unrelated rules would show a yearly Sharpe of about 1.03 by luck; these are about 9 truly different ideas, a softer bar of 0.65.

**Pass every gate: 0. Pass with the softer luck bar only: 0.**

### Best of each kind (by research Sharpe)

| Kind | Rule | Trades | Per year | Halves | Years up | Worst fall | Sharpe | Holdout | Holdout Sharpe | Luck score | Gates failed |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | Known: buy the 20% best 126-day performers (skipping the last 21 days), sell the worst, rebalance every 21 days | 4949 | +12.8% | +62% / +220% | 70% | -81% | 0.50 | -47.3% | -0.66 | 0.01 | 7,8,9 |
| baseline | Known: buy the 20% worst 1-day losers, sell the 20% best, rebalance every 21 day(s) | 9611 | +4.3% | +67% / +28% | 57% | -57% | 0.19 | +107.3% | 1.77 | 0.00 | 3,5,7,9 |
| ours | Ours: Kalman pairs, 5 most correlated pairs re-picked quarterly, enter at z=1.5, exit at z=0.0, max 20 days | 5145 | -0.5% | -2% / -8% | 30% | -13% | -0.26 | -2.4% | -0.68 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours: QuantRank (25 measures, ridge learned on past years only), long top 10% / short bottom 10%, rebalance every 1 day(s) | 103163 | -6.4% | +25% / -167% | 39% | -231% | -0.25 | -2.3% | -0.03 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours: QuantRank (25 measures, ridge learned on past years only), long top 10% / short bottom 10%, rebalance every 21 day(s) | 3477 | +7.8% | -42% / +212% | 52% | -152% | 0.24 | +61.6% | 0.54 | 0.00 | 2,3,7,9 |
| ours | Ours: QuantRank (25 measures, ridge learned on past years only), long top 10% / short bottom 10%, rebalance every 5 day(s) | 19075 | +7.3% | +69% / +92% | 48% | -89% | 0.26 | +43.7% | 0.48 | 0.00 | 3,7,9 |
| ours | Ours, long only: QuantRank top 10%, rebalance every 1 day(s) (measured as the extra over holding all stocks) | 63863 | -2.2% | +50% / -98% | 30% | -118% | -0.15 | -7.5% | -0.19 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours, long only: QuantRank top 10%, rebalance every 21 day(s) (measured as the extra over holding all stocks) | 4744 | +6.7% | -9% / +156% | 43% | -109% | 0.30 | +48.7% | 0.63 | 0.00 | 2,3,7,9 |
| ours | Ours, long only: QuantRank top 10%, rebalance every 5 day(s) (measured as the extra over holding all stocks) | 13201 | +9.0% | +41% / +156% | 61% | -53% | 0.50 | +51.3% | 0.85 | 0.01 | 7 |

**Ours vs the known strategies:** our best (Ours, long only: QuantRank top 10%, rebalance every 5 day(s) (measured as the ex...) research Sharpe 0.50, holdout +51.3%; best known (Known: buy the 20% best 126-day performers (skipping the last 21 days), sell the...) research Sharpe 0.50, holdout -47.3%.

### What QuantRank learned (weights for the latest year, 5-day version)

Positive = higher rank predicts a better next week; negative = it predicts a worse one.

| Measure | Weight |
|---|---|
| mom12_1 | +0.0022 |
| mom6_1 | +0.0015 |
| ret5 | -0.0009 |
| ibs | -0.0009 |
| vol63 | +0.0007 |
| ret2 | -0.0006 |
| hurst | +0.0006 |
| ret1 | -0.0006 |
| beta | +0.0005 |
| rsi2 | -0.0005 |
## India NIFTY 50 stocks

51 stocks, 2007-09-17 to 2026-11-09. Research until 9 Nov 2024, holdout after. 74 variants.

| Gate (on top of the ones before) | Pass alone | Still standing |
|---|---|---|
| 1. At least 100 trades | 74 | 74 |
| 2. Both halves profitable | 6 | 6 |
| 3. 60% of years profitable | 3 | 2 |
| 4. Profitable at double costs | 7 | 2 |
| 5. 60% of its neighbours profitable | 0 | 0 |
| 6. Walk-forward profitable | 4 | 0 |
| 7. Beats the luck bar | 0 | 0 |
| 8. Holdout (since 10 Nov 2024) profitable | 14 | 0 |
| 9. Monte Carlo 95% | 0 | 0 |

Luck bar: best of 74 unrelated rules would show a yearly Sharpe of about 2.28 by luck; these are about 10 truly different ideas, a softer bar of 1.48.

**Pass every gate: 0. Pass with the softer luck bar only: 0.**

### Best of each kind (by research Sharpe)

| Kind | Rule | Trades | Per year | Halves | Years up | Worst fall | Sharpe | Holdout | Holdout Sharpe | Luck score | Gates failed |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | Known: buy the 10% best 252-day performers (skipping the last 21 days), sell the worst, rebalance every 21 days | 1066 | +8.2% | +66% / +62% | 59% | -99% | 0.30 | +24.2% | 0.59 | 0.00 | 3,5,6,7,9 |
| baseline | Known: buy the 10% worst 2-day losers, sell the 10% best, rebalance every 5 day(s) | 12710 | +2.0% | +134% / -103% | 59% | -120% | 0.08 | -45.0% | -0.99 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours: Kalman pairs, 5 most correlated pairs re-picked quarterly, enter at z=2.0, exit at z=0.0, max 20 days | 1713 | -0.7% | +3% / -14% | 29% | -18% | -0.38 | +0.9% | 0.43 | 0.00 | 2,3,4,5,6,7,9 |
| ours | Ours: QuantRank (25 measures, ridge learned on past years only), long top 20% / short bottom 20%, rebalance every 1 day(s) | 46431 | -16.7% | -1% / -261% | 24% | -293% | -1.02 | -84.0% | -2.84 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours: QuantRank (25 measures, ridge learned on past years only), long top 10% / short bottom 10%, rebalance every 21 day(s) | 2193 | +4.1% | +57% / +8% | 53% | -68% | 0.21 | -11.3% | -0.33 | 0.00 | 3,5,7,8,9 |
| ours | Ours: QuantRank (25 measures, ridge learned on past years only), long top 10% / short bottom 10%, rebalance every 5 day(s) | 10228 | -1.7% | +46% / -72% | 35% | -99% | -0.08 | -8.6% | -0.25 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours, long only: QuantRank top 10%, rebalance every 1 day(s) (measured as the extra over holding all stocks) | 16828 | -14.2% | -11% / -211% | 12% | -255% | -1.09 | -30.1% | -1.17 | 0.00 | 2,3,4,5,6,7,8,9 |
| ours | Ours, long only: QuantRank top 20%, rebalance every 21 day(s) (measured as the extra over holding all stocks) | 1998 | +1.8% | +43% / -15% | 35% | -46% | 0.20 | +3.9% | 0.24 | 0.00 | 2,3,5,6,7,9 |
| ours | Ours, long only: QuantRank top 20%, rebalance every 5 day(s) (measured as the extra over holding all stocks) | 8892 | -1.3% | +32% / -53% | 35% | -63% | -0.14 | -8.9% | -0.57 | 0.00 | 2,3,4,5,6,7,8,9 |

**Ours vs the known strategies:** our best (Ours: QuantRank (25 measures, ridge learned on past years only), long top 10% / ...) research Sharpe 0.21, holdout -11.3%; best known (Known: buy the 10% best 252-day performers (skipping the last 21 days), sell the...) research Sharpe 0.30, holdout +24.2%.

### What QuantRank learned (weights for the latest year, 5-day version)

Positive = higher rank predicts a better next week; negative = it predicts a worse one.

| Measure | Weight |
|---|---|
| ret2 | -0.0022 |
| ret5 | -0.0022 |
| rsi2 | -0.0019 |
| resid_z | -0.0018 |
| resid5 | -0.0018 |
| ibs | -0.0016 |
| ret10 | -0.0016 |
| mom12_1 | +0.0015 |
| ret1 | -0.0015 |
| intraday | -0.0015 |

## Caveat

The stock lists are today's names. Stocks that were removed or failed over 25 years are missing, which makes the past look better than it was, most of all for long-only rules.

