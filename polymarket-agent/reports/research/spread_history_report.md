# Spread book, long history (real option prices, Feb 2024 - Nov 2026)

Live rule: 11:00 NY sell the first call >= 0.6% above SPY/QQQ, buy $2 higher, skip credits under $0.05, buy back 15:50. Each leg charged max(3%, $0.02) against us.

- Spreads: **715** on 453 days (2024-02-01 to 2026-11-10), 77% won
- Total **$-2,669** per 1 spread each of SPY and QQQ; per trading day $-5.89; yearly Sharpe -1.31
- Research (2024-02-01 to 2026-03-30): $-2,141, halves $-4 / $-2,137, months up 38%
- **Holdout** (2026-03-31 to 2026-11-10): $-528 over 114 days
- At double costs (research): $-4,669
- Worst day $-355; worst fall from a peak $-3,352; Monte Carlo 4%
- Walk-forward over the 9 settings: $-2,938

| Gate | Result |
|---|---|
| 1 100+ trades | pass |
| 2 both halves + | **FAIL** |
| 3 60% of months + | **FAIL** |
| 4 profit at 2x costs | **FAIL** |
| 5 nearby settings 60% + | **FAIL** |
| 6 walk-forward + | **FAIL** |
| 8 holdout + | **FAIL** |
| 9 Monte Carlo 95% | **FAIL** |

### Nearby settings (research period)

| Distance | Width | Research P&L |
|---|---|---|
| 0.6% | $2 (live) | $-2,141 |
| 0.4% | $1 | $-4,139 |
| 0.4% | $2 | $-2,894 |
| 0.4% | $3 | $-2,142 |
| 0.6% | $1 | $-2,284 |
| 0.6% | $3 | $-1,233 |
| 0.8% | $1 | $-1,634 |
| 0.8% | $2 | $-1,710 |
| 0.8% | $3 | $-1,407 |

### By year

| Year | Spreads | P&L |
|---|---|---|
| 2024 | 231 | $-301 |
| 2025 | 248 | $-1,457 |
| 2026 | 236 | $-911 |

### Worst 8 spreads

| Day | Fund | P&L | Credit | Paid back |
|---|---|---|---|---|
| 2024-04-11 | QQQ | $-192 | $0.08 | $2.00 |
| 2025-11-25 | QQQ | $-182 | $0.13 | $1.95 |
| 2024-09-11 | SPY | $-180 | $0.20 | $2.00 |
| 2026-03-03 | SPY | $-179 | $0.21 | $2.00 |
| 2026-03-31 | SPY | $-177 | $0.23 | $2.00 |
| 2026-03-09 | SPY | $-176 | $0.24 | $2.00 |
| 2024-09-11 | QQQ | $-175 | $0.25 | $2.00 |
| 2026-03-09 | QQQ | $-175 | $0.25 | $2.00 |

Verdict: **FAIL** (failed: 2 both halves +, 3 60% of months +, 4 profit at 2x costs, 5 nearby settings 60% +, 6 walk-forward +, 8 holdout +, 9 Monte Carlo 95%).
