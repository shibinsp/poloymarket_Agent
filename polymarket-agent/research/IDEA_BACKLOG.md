# Idea backlog

Untested ideas, best first. The daily idea lab takes the top **NEXT** item, tests
it with the `strategy-lab` skill (all 9 gates plus the trap checks), records the
verdict in `LAB_NOTEBOOK.md`, marks it here as DONE with the verdict, and adds
one or two new ideas (variations of near-misses, or ideas from research papers
or from what the user sends). Ideas the user sends go to the top.

Data we already have: US ETFs daily 2001-2026 (scratchpad `d20/`), 90 US stocks
daily 2001-2026 and 5-min May-Nov 2026 (`research/data/`), QQQ 5-min 2016-2026,
NIFTY 50 stocks + NIFTY/BANKNIFTY/India VIX daily 2007-2026, SPY/QQQ 0DTE option
bars Feb 2024 onward (`research/data/spreads/`), FOMC dates 2001-2026
(scratchpad `fomc_dates.json`), 30,591 resolved Polymarket markets.

| # | Status | Idea | Market | Why it might work |
|---|---|---|---|---|
| 1 | DONE 11 Nov: **PASS** | Pre-FOMC drift: buy SPY at the close the day before a Fed decision, sell at the decision day's close | US | Documented in research (Lucca & Moench 2015) |
| 2 | DONE 11 Nov: FAIL | India "Turnaround Monday": NIFTY below its 25-day average on Friday -> buy Monday open, sell Tuesday close | India | Turnaround Tuesday passed in the US; does it carry over? |
| 2a | NEXT | Pre-FOMC drift, finer: hold only until the 14:00 announcement (5-min data, QQQ 2016+), and on ECB/RBI decision days | US / India | Research says the drift is before the announcement |
| 3 | NEXT | Turnaround Tuesday on more US funds (QQQ, IWM, sector ETFs) and as a 2-fund basket | US | Neighbours were all positive; more trades = more proof |
| 4 | NEXT | Pre-holiday drift: buy SPY the close 2 days before a market holiday, sell the day before | US | Classic calendar effect |
| 5 | NEXT | Sector momentum: each month hold the 3 best sector ETFs of the last 6 months (skip 1) | US | Cross-sectional sector momentum, not yet tested |
| 6 | NEXT | Dual momentum: SPY vs EFA vs IEF, monthly, hold the best if above cash | US | Known long-term rule; lower falls than holding |
| 7 | NEXT | Put credit spreads and iron condors on SPY/QQQ 0DTE (real option prices) | US options | The call spread book is our best intraday book; test the other side |
| 8 | NEXT | Weekly expiry day (Tuesday) effect on NIFTY: direction/size of the move | India | Expiry-day flows |
| 9 | NEXT | Overnight vs day split on NIFTY: hold close->open only | India | US index gains are overnight; is India the same? |
| 10 | NEXT | BTC weekend effect: hold BTC only Fri close -> Mon open, or only weekdays | Crypto | Weekend liquidity |
| 11 | NEXT | IBS on DIA and sector ETFs (XLK passed the old screen), combined with the IBS book | US | Extend the one 25-year winner |
| 12 | NEXT | VIX spike reversal: buy SPY when VIX jumps 20%+ in a day, hold 5 days | US | Fear overshoots |
| 13 | NEXT | Post-earnings drift on the 90 stocks (needs earnings dates from SEC filings) | US | Documented effect |
| 14 | NEXT | Polymarket: favourite-longshot in politics markets only, priced at the real ask (shadow data) | Polymarket | Politics books are deeper than small sports |
