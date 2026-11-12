# Polymarket full re-test

Every resolved market with a scheduled end from 15 Aug to 8 Nov 2026, any size: 30,591 markets.
Rules use only what was known when the bet would be placed. Holdout: markets ending after 18 Oct.

Market types: crypto 39, esports 168, match 25,082, minor tennis 156, other 3,368, politics 1,778

2220 rule variants. Luck bar: Sharpe per bet 0.271 (strict), 0.092 (softer, about 4 distinct ideas).

| Gate (on top of the ones before) | Pass alone | Still standing |
|---|---|---|
| 1. At least 100 bets | 562 | 562 |
| 2. Both halves profitable | 366 | 137 |
| 3. 60% of 2-week blocks profitable | 238 | 88 |
| 4. Profitable at 2c cost | 598 | 81 |
| 5. 60% of neighbours profitable | 445 | 81 |
| 6. Walk-forward profitable | 760 | 71 |
| 7. Beats the luck bar | 65 | 0 |
| 8. Holdout profitable (20+ bets) | 367 | 0 |
| 9. Monte Carlo 95% | 41 | 0 |

**Pass every gate: 0. With the softer luck bar only: 4.**

### Candidates

| Rule | Bets | Won | Avg per bet | Halves ($ per $1 bets) | Blocks up | At 2c | Holdout bets | Holdout | Luck score | Monte Carlo |
|---|---|---|---|---|---|---|---|---|---|---|
| all: buy the side priced 50-60c, 168 h before the scheduled end, any activity | 3356 | 59% | +11.5% | +147.1 / +239.3 | 83% | +317.4 | 469 | +44.9 | 0.00 | 100% |
| all: buy the side priced 50-60c, 168 h before the scheduled end, 6+ price updates in the last day | 3356 | 59% | +11.5% | +147.1 / +239.3 | 83% | +317.4 | 469 | +44.9 | 0.00 | 100% |
| match: buy the side priced 50-60c, 168 h before the scheduled end, any activity, side fell 3c+ in the last day | 311 | 64% | +19.0% | +28.6 / +30.4 | 83% | +52.2 | 44 | +9.1 | 0.17 | 100% |
| match: buy the side priced 50-60c, 168 h before the scheduled end, 6+ price updates in the last day, side fell 3c+ in the last day | 311 | 64% | +19.0% | +28.6 / +30.4 | 83% | +52.2 | 44 | +9.1 | 0.17 | 100% |

### Closest rules (pass gates 1-6, best research Sharpe)

| Rule | Bets | Won | Avg per bet | Halves ($ per $1 bets) | Blocks up | At 2c | Holdout bets | Holdout | Luck score | Monte Carlo |
|---|---|---|---|---|---|---|---|---|---|---|
| all: buy the side priced 60-70c, 72 h before the scheduled end, any activity, side fell 3c+ in the last day | 103 | 77% | +17.0% | +10.1 / +7.4 | 83% | +15.7 | 40 | +7.9 | 0.47 | 100% |
| all: buy the side priced 60-70c, 72 h before the scheduled end, 6+ price updates in the last day, side fell 3c+ in the last day | 103 | 77% | +17.0% | +10.1 / +7.4 | 83% | +15.7 | 40 | +7.9 | 0.47 | 99% |
| match: buy the side priced 50-60c, 168 h before the scheduled end, any activity, side fell 3c+ in the last day | 311 | 64% | +19.0% | +28.6 / +30.4 | 83% | +52.2 | 44 | +9.1 | 0.17 | 100% |
| match: buy the side priced 50-60c, 168 h before the scheduled end, 6+ price updates in the last day, side fell 3c+ in the last day | 311 | 64% | +19.0% | +28.6 / +30.4 | 83% | +52.2 | 44 | +9.1 | 0.17 | 100% |
| other: buy the side priced 80-90c, 168 h before the scheduled end, any activity | 127 | 92% | +6.6% | +1.5 / +6.9 | 67% | +6.9 | 46 | +2.5 | 0.31 | 99% |
| other: buy the side priced 80-90c, 168 h before the scheduled end, 6+ price updates in the last day | 127 | 92% | +6.6% | +1.5 / +6.9 | 67% | +6.9 | 46 | +2.5 | 0.31 | 99% |
| match: buy the side priced 50-60c, 24 h before the scheduled end, any activity, side fell 3c+ in the last day | 179 | 65% | +18.3% | +21.0 / +11.7 | 83% | +28.9 | 56 | +1.6 | 0.22 | 100% |
| match: buy the side priced 50-60c, 24 h before the scheduled end, 6+ price updates in the last day, side fell 3c+ in the last day | 179 | 65% | +18.3% | +21.0 / +11.7 | 83% | +28.9 | 56 | +1.6 | 0.22 | 100% |
| match: buy the side priced 50-60c, 72 h before the scheduled end, any activity, side fell 3c+ in the last day | 158 | 65% | +17.4% | +24.4 / +3.1 | 83% | +24.2 | 26 | -2.1 | 0.20 | 0% |
| match: buy the side priced 50-60c, 72 h before the scheduled end, 6+ price updates in the last day, side fell 3c+ in the last day | 158 | 65% | +17.4% | +24.4 / +3.1 | 83% | +24.2 | 26 | -2.1 | 0.20 | 0% |
| match: buy the side priced 70-80c, 72 h before the scheduled end, any activity, side rose 3c+ in the last day | 204 | 83% | +9.7% | +8.1 / +11.7 | 67% | +16.8 | 62 | +13.4 | 0.18 | 100% |
| match: buy the side priced 70-80c, 72 h before the scheduled end, 6+ price updates in the last day, side rose 3c+ in the last day | 204 | 83% | +9.7% | +8.1 / +11.7 | 67% | +16.8 | 62 | +13.4 | 0.18 | 100% |
| all: buy the side priced 50-60c, 72 h before the scheduled end, any activity, side fell 3c+ in the last day | 188 | 64% | +16.7% | +20.1 / +11.4 | 83% | +27.6 | 43 | -0.7 | 0.15 | 0% |
| all: buy the side priced 50-60c, 72 h before the scheduled end, 6+ price updates in the last day, side fell 3c+ in the last day | 188 | 64% | +16.7% | +20.1 / +11.4 | 83% | +27.6 | 43 | -0.7 | 0.15 | 0% |
| all: buy the side priced 50-60c, 24 h before the scheduled end, any activity, side fell 3c+ in the last day | 219 | 63% | +15.9% | +26.0 / +8.9 | 67% | +30.3 | 75 | +9.6 | 0.10 | 100% |

### The live underdog rule on the full sample

| Rule | Bets | Won | Avg per bet | Halves ($ per $1 bets) | Blocks up | At 2c | Holdout bets | Holdout | Luck score | Monte Carlo |
|---|---|---|---|---|---|---|---|---|---|---|
| all: buy the side priced 20-30c, 168 h before the scheduled end, any activity | 1020 | 24% | -8.2% | -76.0 / -7.1 | 17% | -117.7 | 294 | -58.7 | 0.00 | 0% |
| match: buy the side priced 20-30c, 168 h before the scheduled end, any activity | 854 | 24% | -7.8% | -57.2 / -9.2 | 17% | -95.3 | 205 | -66.2 | 0.00 | 0% |
| minor tennis: buy the side priced 20-30c, 168 h before the scheduled end, any activity | 15 | 27% | -3.4% | -3.7 / +3.2 | 33% | -1.0 | 2 | +1.3 | 0.14 | 0% |
| other: buy the side priced 20-30c, 168 h before the scheduled end, any activity | 90 | 22% | -13.9% | -10.7 / -1.8 | 33% | -15.4 | 48 | +3.5 | 0.00 | 0% |

### Average result per bet by market type and price (168 h before the end, any activity)

| Type | 3-10c | 10-20c | 20-30c | 30-40c | 40-50c | 50-60c | 60-70c | 70-80c | 80-90c | 90-97c |
|---|---|---|---|---|---|---|---|---|---|---|
| all | -32% (539) | -29% (576) | -8% (1020) | -14% (1101) | -25% (2644) | +12% (3356) | +5% (1114) | +1% (1043) | +4% (592) | +0% (539) |
| crypto | - | - | - | - | - | - | - | - | - | - |
| esports | - | - | - | - | - | - | - | - | - | - |
| match | -21% (259) | -28% (376) | -8% (854) | -14% (967) | -25% (2496) | +11% (3183) | +4% (981) | +1% (877) | +3% (391) | -0% (263) |
| minor tennis | - | - | - | - | -21% (42) | +28% (66) | - | - | - | - |
| other | -35% (183) | -43% (127) | -14% (90) | -32% (79) | -29% (83) | +13% (85) | +18% (80) | +1% (91) | +7% (127) | -0% (182) |
| politics | -53% (91) | -1% (63) | -9% (55) | -6% (39) | +8% (27) | -12% (26) | +3% (38) | +4% (54) | -0% (64) | +1% (88) |
