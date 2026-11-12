# Alpha library test (Vibe-Trading zoo: Alpha101, GTJA191, Qlib158, academic)

Run: 2026-11-11 22:29 IST. Research only.

## US stocks (91)

265 formulas run x 3 holding periods = 795 variants. 24 skipped (need sector or other data), 3 failed to run on our data.

| Gate (on top of the ones before) | Pass alone | Still standing |
|---|---|---|
| 1. At least 100 trades | 794 | 794 |
| 2. Both halves profitable | 98 | 97 |
| 3. 60% of years profitable | 56 | 40 |
| 4. Profitable at double costs | 125 | 38 |
| 5. 60% of its neighbours profitable | 132 | 26 |
| 6. Walk-forward profitable | 360 | 25 |
| 7. Beats the luck bar | 0 | 0 |
| 8. Holdout (since 10 Nov 2024) profitable | 240 | 0 |
| 9. Monte Carlo 95% | 6 | 0 |

Luck bar: yearly Sharpe 1.50 (strict), 0.83 (softer, about 15 distinct ideas).

**Single formulas passing every gate: 0; with the softer bar only: 0.**

### Top 15 single formulas by research Sharpe

| Formula | Trades | Per year | Halves | Years up | Sharpe | Holdout | Holdout Sharpe | Gates failed |
|---|---|---|---|---|---|---|---|---|
| gtja191_143 (momentum): long top 10% / short bottom 10%, every 21 day(s) | 52 | +20.0% | +149% / +289% | 74% | 0.64 | -24.4% | -0.42 | 1,7,8,9 |
| gtja191_143 (momentum): long top 10% / short bottom 10%, every 5 day(s) | 104 | +19.5% | +149% / +280% | 74% | 0.62 | -24.6% | -0.42 | 7,8,9 |
| gtja191_143 (momentum): long top 10% / short bottom 10%, every 1 day(s) | 168 | +19.1% | +142% / +277% | 74% | 0.61 | -26.9% | -0.46 | 7,8,9 |
| qlib158_klow2 (microstructure): long top 10% / short bottom 10%, every 21 day(s) | 6240 | +11.8% | +61% / +197% | 74% | 0.57 | +64.3% | 1.34 | 5,7,9 |
| academic_carhart_mom (momentum): long top 10% / short bottom 10%, every 21 day(s) | 1969 | +18.6% | +81% / +328% | 70% | 0.50 | +71.5% | 0.74 | 7 |
| academic_carhart_mom (momentum): long top 10% / short bottom 10%, every 5 day(s) | 4209 | +16.5% | +71% / +292% | 70% | 0.44 | +79.5% | 0.83 | 7 |
| academic_carhart_mom (momentum): long top 10% / short bottom 10%, every 1 day(s) | 9975 | +14.5% | +44% / +274% | 70% | 0.38 | +56.2% | 0.60 | 7 |
| gtja191_079 (momentum): long top 10% / short bottom 10%, every 21 day(s) | 5866 | +9.5% | +71% / +137% | 52% | 0.36 | -43.7% | -0.70 | 3,5,7,8,9 |
| gtja191_071 (reversal): long top 10% / short bottom 10%, every 21 day(s) | 5589 | +11.4% | +70% / +180% | 61% | 0.33 | -21.1% | -0.27 | 7,8,9 |
| qlib158_vstd10 (volume, volatility): long top 10% / short bottom 10%, every 21 day(s) | 6023 | +7.7% | +53% / +115% | 61% | 0.33 | +36.8% | 0.77 | 7 |
| qlib158_vsumn30 (volume, volatility): long top 10% / short bottom 10%, every 21 day(s) | 6418 | +7.4% | +72% / +90% | 70% | 0.33 | -13.8% | -0.29 | 5,7,8,9 |
| gtja191_118 (reversal): long top 10% / short bottom 10%, every 21 day(s) | 6059 | +7.9% | +106% / +68% | 70% | 0.32 | -16.3% | -0.26 | 7,8,9 |
| qlib158_wvma20 (volume, volatility): long top 10% / short bottom 10%, every 5 day(s) | 13799 | +6.7% | +90% / +56% | 65% | 0.31 | +36.2% | 0.85 | 7 |
| qlib158_vstd5 (volume, volatility): long top 10% / short bottom 10%, every 21 day(s) | 6142 | +7.1% | +38% / +117% | 74% | 0.31 | +77.5% | 1.59 | 5,7,9 |
| gtja191_063 (momentum): long top 10% / short bottom 10%, every 21 day(s) | 6125 | +8.0% | +66% / +110% | 61% | 0.31 | -51.7% | -0.86 | 5,7,8,9 |

Across the library: 26% of variants were profitable in the research years and 30% in the holdout. The top 10% by research Sharpe made -9.5% on average in the holdout (all variants: -17.6%). If the best in research stay best in the holdout, the library has signal.

### Our combination of the best 20 formulas (chosen and weighted on past years only)

| Rule | Trades | Per year | Halves | Years up | Sharpe | Holdout | Gates failed | Beta-adjusted research / holdout |
|---|---|---|---|---|---|---|---|---|
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 10% / short bottom 10%, every 1 day(s) | 92176 | -16.9% | -39% / -332% | 30% | -0.64 | -34.6% | 2,3,4,5,6,7,8,9 | -21.7% / -25.0% |
| Ours, long only: best 20 formulas combined, top 10% minus holding all stocks, every 1 day(s) | 208087 | -8.6% | -3% / -186% | 26% | -0.47 | -4.0% | 2,3,4,5,6,7,8,9 | -11.7% / -7.0% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 20% / short bottom 20%, every 1 day(s) | 157283 | -13.7% | -17% / -284% | 22% | -0.67 | -36.4% | 2,3,4,5,6,7,8,9 | -17.6% / -25.0% |
| Ours, long only: best 20 formulas combined, top 20% minus holding all stocks, every 1 day(s) | 220695 | -6.6% | +6% / -150% | 26% | -0.52 | -4.1% | 2,3,4,5,6,7,8,9 | -8.9% / -6.2% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 10% / short bottom 10%, every 5 day(s) | 21523 | +0.4% | +95% / -85% | 43% | 0.02 | +48.8% | 2,3,4,5,6,7,9 | -4.0% / +15.4% |
| Ours, long only: best 20 formulas combined, top 10% minus holding all stocks, every 5 day(s) | 42882 | +0.5% | +72% / -61% | 48% | 0.03 | +38.5% | 2,3,4,5,6,7,9 | -2.2% / +14.1% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 20% / short bottom 20%, every 5 day(s) | 36419 | +0.5% | +94% / -84% | 39% | 0.02 | -9.9% | 2,3,4,5,6,7,8,9 | -3.4% / -12.4% |
| Ours, long only: best 20 formulas combined, top 20% minus holding all stocks, every 5 day(s) | 46246 | +0.0% | +56% / -55% | 43% | 0.00 | -5.6% | 2,3,4,5,6,7,8,9 | -2.2% / -6.9% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 10% / short bottom 10%, every 21 day(s) | 5324 | -2.4% | +89% / -143% | 48% | -0.09 | +42.3% | 2,3,4,5,6,7,9 | -6.7% / +14.0% |
| Ours, long only: best 20 formulas combined, top 10% minus holding all stocks, every 21 day(s) | 10994 | -1.9% | +57% / -98% | 48% | -0.10 | +24.0% | 2,3,4,5,6,7,9 | -4.6% / +5.8% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 20% / short bottom 20%, every 21 day(s) | 8885 | -0.0% | +50% / -50% | 43% | -0.00 | +11.3% | 2,3,4,5,6,7,9 | -3.6% / -1.6% |
| Ours, long only: best 20 formulas combined, top 20% minus holding all stocks, every 21 day(s) | 11721 | -0.9% | +9% / -29% | 39% | -0.08 | +18.5% | 2,3,4,5,6,7,9 | -3.2% / +4.1% |

Formulas chosen for the latest year: gtja191_143, academic_carhart_mom, qlib158_beta30, qlib158_wvma20, qlib158_beta20, gtja191_135, qlib158_sumd30, qlib158_sump30, qlib158_vstd30, qlib158_beta60, qlib158_rsqr60, gtja191_171, academic_smb, academic_mkt_rf, qlib158_wvma30, gtja191_071, gtja191_118, gtja191_144, qlib158_max10, qlib158_kup2

## India NIFTY 50 stocks

266 formulas run x 3 holding periods = 798 variants. 27 skipped (need sector or other data), 3 failed to run on our data.

| Gate (on top of the ones before) | Pass alone | Still standing |
|---|---|---|
| 1. At least 100 trades | 796 | 796 |
| 2. Both halves profitable | 29 | 27 |
| 3. 60% of years profitable | 21 | 17 |
| 4. Profitable at double costs | 29 | 16 |
| 5. 60% of its neighbours profitable | 54 | 12 |
| 6. Walk-forward profitable | 132 | 12 |
| 7. Beats the luck bar | 0 | 0 |
| 8. Holdout (since 10 Nov 2024) profitable | 86 | 0 |
| 9. Monte Carlo 95% | 3 | 0 |

Luck bar: yearly Sharpe 3.77 (strict), 2.25 (softer, about 20 distinct ideas).

**Single formulas passing every gate: 0; with the softer bar only: 0.**

### Top 15 single formulas by research Sharpe

| Formula | Trades | Per year | Halves | Years up | Sharpe | Holdout | Holdout Sharpe | Gates failed |
|---|---|---|---|---|---|---|---|---|
| gtja191_144 (liquidity): long top 10% / short bottom 10%, every 21 day(s) | 1127 | +25.9% | +267% / +138% | 94% | 1.14 | -7.8% | -0.31 | 7,8,9 |
| academic_illiq (liquidity): long top 10% / short bottom 10%, every 21 day(s) | 925 | +24.0% | +266% / +110% | 94% | 1.07 | -4.5% | -0.18 | 7,8,9 |
| gtja191_144 (liquidity): long top 10% / short bottom 10%, every 5 day(s) | 2356 | +19.1% | +218% / +80% | 82% | 0.86 | -7.5% | -0.31 | 7,8,9 |
| academic_illiq (liquidity): long top 10% / short bottom 10%, every 5 day(s) | 1757 | +18.4% | +230% / +58% | 88% | 0.82 | -8.1% | -0.32 | 7,8,9 |
| academic_smb (quality): long top 10% / short bottom 10%, every 5 day(s) | 746 | +17.9% | +172% / +108% | 88% | 0.72 | +15.4% | 0.58 | 7 |
| academic_smb (quality): long top 10% / short bottom 10%, every 1 day(s) | 1068 | +17.1% | +169% / +98% | 88% | 0.68 | +10.6% | 0.40 | 7 |
| academic_illiq (liquidity): long top 10% / short bottom 10%, every 1 day(s) | 3512 | +14.8% | +172% / +60% | 82% | 0.67 | -10.1% | -0.39 | 7,8,9 |
| gtja191_144 (liquidity): long top 10% / short bottom 10%, every 1 day(s) | 4946 | +14.9% | +180% / +53% | 71% | 0.67 | -14.5% | -0.56 | 7,8,9 |
| academic_smb (quality): long top 10% / short bottom 10%, every 21 day(s) | 482 | +15.7% | +152% / +94% | 82% | 0.63 | +20.4% | 0.77 | 7 |
| gtja191_143 (momentum): long top 10% / short bottom 10%, every 21 day(s) | 34 | +17.1% | +126% / +141% | 59% | 0.54 | +1.2% | 0.03 | 1,3,7,9 |
| gtja191_143 (momentum): long top 10% / short bottom 10%, every 1 day(s) | 102 | +16.5% | +122% / +136% | 59% | 0.52 | +0.8% | 0.02 | 3,7,9 |
| gtja191_143 (momentum): long top 10% / short bottom 10%, every 5 day(s) | 60 | +16.4% | +120% / +136% | 59% | 0.52 | +1.3% | 0.04 | 1,3,7,9 |
| qlib158_klen (microstructure): long top 10% / short bottom 10%, every 21 day(s) | 2864 | +11.1% | +133% / +40% | 71% | 0.47 | -14.8% | -0.39 | 5,7,8,9 |
| gtja191_158 (volatility): long top 10% / short bottom 10%, every 21 day(s) | 2863 | +10.8% | +129% / +41% | 71% | 0.45 | -11.6% | -0.31 | 5,7,8,9 |
| qlib158_max60 (momentum): long top 10% / short bottom 10%, every 21 day(s) | 2429 | +9.8% | +103% / +51% | 65% | 0.35 | +16.7% | 0.43 | 7,9 |

Across the library: 11% of variants were profitable in the research years and 11% in the holdout. The top 10% by research Sharpe made -14.7% on average in the holdout (all variants: -48.9%). If the best in research stay best in the holdout, the library has signal.

### Our combination of the best 20 formulas (chosen and weighted on past years only)

| Rule | Trades | Per year | Halves | Years up | Sharpe | Holdout | Gates failed | Beta-adjusted research / holdout |
|---|---|---|---|---|---|---|---|---|
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 10% / short bottom 10%, every 1 day(s) | 25998 | -12.5% | -0% / -196% | 24% | -0.56 | -49.9% | 2,3,4,5,6,7,8,9 | -16.1% / -26.4% |
| Ours, long only: best 20 formulas combined, top 10% minus holding all stocks, every 1 day(s) | 12980 | -4.1% | -10% / -55% | 29% | -0.29 | -10.1% | 2,3,4,5,6,7,8,9 | -6.2% / -5.5% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 20% / short bottom 20%, every 1 day(s) | 39514 | -11.5% | -6% / -174% | 18% | -0.68 | -56.5% | 2,3,4,5,6,7,8,9 | -14.5% / -29.6% |
| Ours, long only: best 20 formulas combined, top 20% minus holding all stocks, every 1 day(s) | 19786 | -5.7% | -24% / -66% | 12% | -0.60 | -24.3% | 2,3,4,5,6,7,8,9 | -7.5% / -12.8% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 10% / short bottom 10%, every 5 day(s) | 9793 | +0.3% | +78% / -73% | 41% | 0.01 | -13.7% | 2,3,4,5,6,7,8,9 | -2.7% / -7.9% |
| Ours, long only: best 20 formulas combined, top 10% minus holding all stocks, every 5 day(s) | 5960 | +0.5% | +35% / -27% | 47% | 0.04 | -10.5% | 2,3,4,5,6,7,8,9 | -1.3% / -5.7% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 20% / short bottom 20%, every 5 day(s) | 15216 | +0.6% | +82% / -72% | 41% | 0.04 | -26.9% | 2,3,4,5,6,7,8,9 | -2.2% / -14.4% |
| Ours, long only: best 20 formulas combined, top 20% minus holding all stocks, every 5 day(s) | 8796 | -1.4% | +27% / -48% | 41% | -0.15 | -19.4% | 2,3,4,5,6,7,8,9 | -3.0% / -10.2% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 10% / short bottom 10%, every 21 day(s) | 2617 | +1.1% | +29% / -11% | 35% | 0.06 | +16.0% | 2,3,4,5,6,7,9 | -1.9% / +7.7% |
| Ours, long only: best 20 formulas combined, top 10% minus holding all stocks, every 21 day(s) | 2379 | +0.6% | +19% / -10% | 35% | 0.04 | +9.5% | 2,3,4,5,6,7,9 | -0.9% / +4.6% |
| Ours: best 20 library formulas (chosen on past years only), ridge-combined, long top 20% / short bottom 20%, every 21 day(s) | 4194 | -0.6% | +35% / -44% | 41% | -0.04 | -3.4% | 2,3,4,5,6,7,8,9 | -3.1% / -2.2% |
| Ours, long only: best 20 formulas combined, top 20% minus holding all stocks, every 21 day(s) | 3175 | +0.8% | +27% / -14% | 41% | 0.09 | -8.2% | 2,3,4,5,6,7,8,9 | -0.3% / -4.4% |

Formulas chosen for the latest year: gtja191_144, academic_illiq, academic_smb, gtja191_143, qlib158_max60, gtja191_047, qlib158_std60, gtja191_072, qlib158_vstd30, gtja191_082, qlib158_qtlu10, qlib158_vstd60, qlib158_max20, gtja191_065, academic_carhart_mom, gtja191_034, qlib158_ma10, qlib158_qtlu60, gtja191_165, qlib158_cntn60

