# Lab notebook

Every trading idea we test goes here, passed or failed. A failure is a result:
it tells us where the money is not, so we never pay to learn it twice.

How to add an entry: use the `lab-notebook` skill, or copy the template at the
bottom. Newest first inside each section. Verdicts:

- **PASS**: cleared every gate. Goes to paper trading, never straight to money.
- **WATCH**: passes some gates; worth re-testing with more data.
- **FAIL**: a gate it cannot pass. The "why" says which one.
- **LIVE-FAIL**: passed a backtest, then failed live. The most useful kind of
  lesson, because it shows how our testing fooled us.

## What we have learned so far (read this first)

1. **Daily IBS on index funds is the only rule with 25 years of support.**
   Everything intraday so far is thin or fails.
2. **Sample selection lies.** The Polymarket underdog test used only markets that
   ended with high volume; live, the bot bet on small tennis matches the test
   never saw. Always test on the same population the bot will trade.
3. **Data source matters.** The US ORB was tested on SIP volume but picked
   stocks on IEX live; the two shared only 29% of picks.
4. **News, mood and AI forecasts have never improved a rule here.** News exits,
   mood filters, TimesFM direction calls (50.3%) and bad-day prediction all
   failed or reversed between markets.
5. **Costs kill frequent trading.** Crypto breakouts, overnight holds and India
   options died on fees and slippage.
6. **Profit from a few lottery wins is not an edge.** The US options book's
   profit came from far out-of-the-money wins; without the best 3 trades it lost.
7. **Four to six months of intraday data cannot prove an intraday rule.** In the
   11 Nov search, the US rules that looked best in May-Sep were a coin flip in
   the next 40 days. Intraday ideas need years of minute data before we can
   trust them; the weekly run adds data every week.
8. **Always measure the extra after removing market risk and the top 3 winners.**
   QuantRank's "+9% a year" was a tilt toward high-risk stocks plus MU, SHOP
   and AMD. A strategy that only beats holding everything because it holds
   riskier things is not an edge.
9. **Back-adjusted prices leak the future into price-level formulas.** Yahoo's
   25-year prices are adjusted backwards for splits and dividends, so stocks that
   later grew show tiny early prices. Any rule that uses a price in dollars (not
   a ratio) can "predict" winners. Test every formula with a fake split; if its
   picks move, throw it out.
10. **The target drives risk.** Rs 500 a day on about Rs 2 lakh needs about 62% a
   year. Judge by percent a month against buy-and-hold, not by rupees a day.

## US

| Date | Idea | Data | Result | Verdict | Why |
|---|---|---|---|---|---|
| 2026-11-12 | Pre-FOMC drift, exit time: buy QQQ at the close before a Fed decision, sell at 12:00 / 13:00 / **13:55 (before the 14:00 statement)** / 15:00 / close | QQQ 5-min SIP 2016-2026, 86 meetings, 2 bp a side (`research/pipeline/ideas/prefomc_intraday.py`) | 14:00 exit: 65% won, +0.31%/trade, holdout +7.0%, 8/11 years up. Close exit (the live rule): 55% won, +0.23%, holdout +1.0%, 6/11 years. The statement-to-close part alone: -0.07% a meeting with 1.17% swings. Placebo a week later -0.06%. Without best 3: +17.6% of +26.2%; without best 10: +3.2% | WATCH (better exit) | Matches the research (drift is before the statement). Fails only the 100-trade gate (86 meetings in 10 years) and is thin without its best 10. Proposal: switch the live pre-Fed book's exit to 13:55 NY, with the user's yes |
| 2026-11-11 | **Spread book, long history**: live rule (11:00 sell first call >=0.6% OTM, $2 wide, skip credit <5c, buy back 15:50) on real SPY/QQQ 0DTE option trade bars | Feb 2024 - Nov 2026, 453-943 days depending on cost | Cost decides everything: no bid-ask charge +$3,812 (83% won, avg win $17, avg loss $60); 25% of a 3%/2c half-spread +$1,674; 50% about $0 (-$147); full charge -$2,669 (all 9 neighbours negative, Monte Carlo 4%). Live 9-10 Nov filled better than the full-charge replay (QQQ +$25 vs +$13) | WATCH (marginal) | The edge is about 1c a leg. It only pays if fills are near the mid, and Alpaca *paper* fills may be kinder than real ones. The old +$1,207 (May-Oct 2026) was the no-exit-cost version of an unusually good stretch |
| 2026-11-11 | Spread book live fill quality (`tools/spread_fills.py`, in the hourly report) | 7 live paper spreads, 6-11 Nov | Entry cost averages +2.1c a leg against the quoted mid (break-even about 1c): 0-0.5c on calm fills, 2.5-4.5c when the price moved during the 60-90 s before filling. Exit vs traded price: +1c (2 measurable) | WATCH, leaning FAIL | At +2c a leg the replay says the book loses. Keep measuring; user chose "keep it, measure fills" on 11 Nov |
| 2026-11-11 | **Pre-FOMC drift**: buy SPY at the close the day before a Fed decision, sell at the decision day's close | SPY daily 2002-2026, 200 Fed meetings, 2 bp a side; holdout = last 25% (about 2020 on) | 55% won, +0.26%/trade, PF 2.14, 16/19 years up, all 4 neighbours (QQQ, DIA, IWM, 2-day entry) positive, luck 1.00, MC 100%; without best 3 +38% of +52%; placebo a week later -0.10%/trade. Holdout only +2.6% over 50 trades | **PASS** (weakening; paper from 12 Nov) | Real, Fed-specific, but smaller since the 2015 paper made it famous. Only ~8 trades a year, ~2% a year on the money used. Built as a simulated $1,000 SPY book (`src/us/prefomc.rs`, `/api/us/prefomc`) at the user's yes; first trade 8-9 Dec 2026 |
| 2026-11-11 | India Turnaround: NIFTY Friday below 25-day average -> buy Monday open, sell Tuesday close | NIFTY daily 2007-2026, 5 bp a side | 354 trades, 49% won, PF 0.72, -61%, 6/16 years up, 0/3 neighbours; placebo also negative | FAIL | The US Monday effect does not carry over to India |
| 2026-11-11 | **Turnaround Tuesday** (reel): if Friday's close is below the 25-day SMA, buy DIA at Monday's open, sell at Tuesday's close | DIA daily 2001-2026, 2 bp a side; holdout = last 25% of trades (from Sep 2020) | 416 trades, 56% won, +0.23%/trade, PF 1.52, worst fall -13.9%, 16/20 years up, holdout +18%, all 5 neighbours (SMA 20/30, SPY, QQQ, IWM) positive, luck 0.99, MC 100%. Without best 3: +69% (of +94%). Same rule Wed/Thu loses. ~40% of trades overlap the IBS book | **PASS (paper from 12 Nov)** | First outside idea to clear every gate. Built as a simulated $1,000 DIA book (`src/us/turnaround.rs`, `/api/us/turnaround`) at the user's yes 11 Nov; loads at the 02:35 IST 12 Nov restart; first possible trade Mon 16 Nov |
| 2026-11-11 | Daily long bias (reel): buy QQQ every open, sell the close | QQQ daily 2001-2026 | -0.025%/trade, 6/20 years up; open-to-close +1.9%/yr vs overnight +10.9%/yr | FAIL | US index gains come overnight, not in the day session |
| 2026-11-11 | ORB NASDAQ (reel): 9:30-9:45 range, 5-min close beyond, stop + 0.25 ATR, target 2x range, flat 15:55 | QQQ 5-min SIP 2016-2026, 2 bp a side | 2,712 trades, 46.6% won, PF 0.88, -84.5% total, 2/9 years up | FAIL | Loses on QQQ itself |
| 2026-11-11 | Morning range breakout USDJPY (reel): 03:00-06:00 broker-time range, stop orders, flat 18:00 | Yahoo hourly, Jan 2024-Nov 2026 only | 711 trades, PF 1.18, +15.3%, holdout +2.4%, 6/6 neighbours + | WATCH | Fails the luck bar on 2 years of data; we have no FX broker |
| 2026-11-11 | Alpha library (Vibe-Trading: Alpha101, GTJA191, Qlib158, academic), 431 formulas x 3 holds | 90 stocks daily 2001-2026 | Best single, gtja191_164: +25%/yr beta-adjusted, t=4.5, 22/24 years | LEAK | Its score depends on the price level; on back-adjusted prices that reveals future winners (lowest adjusted price alone "made" +17%/yr, t=2.9, then reversed). Re-run with a fake-split check excludes such formulas |
| 2026-11-11 | Alpha library, leak-free re-run (fake-split check) + walk-forward combination of the best 20 | US 90 / India 51 stocks daily | 166 of 431 US formulas (162 of 428 India) were price-level leaks. Clean: 0 single formulas pass; the combination makes +0.5%/yr with the second half negative; the earlier "+6.4% beta-adjusted" combo was the leak | FAIL (all) | Published formulas add nothing on our stocks after costs once the leak is removed |
| 2026-11-11 | QuantRank (our own model): 25 measures (returns, vol, z-scores, IBS, RSI, beta residuals, Hurst, OU half-life, Kalman slope, volume) ranked daily, ridge weights learned walk-forward by year; 74 variants with baselines and pairs | 90 stocks daily 2001-2026, 5 bp a side, holdout 10 Nov 2024+ | Best: long top 10% weekly, +9%/yr over holding all stocks, holdout +51%, fails only the luck bar. Beta-adjusted: 0%/yr in research; holdout extra is beta 0.58 + MU/SHOP/AMD (35 of 47 points); without them -4%/yr | FAIL | High-risk tilt plus 3 lucky stocks, not skill. Learned weights = 12-month momentum + short-term reversal, as in finance research |
| 2026-11-11 | Kalman pairs, top 5/10 correlated pairs re-picked quarterly, z 1.5-2.5 | as above | -0.5%/yr, 30% of years up | FAIL | Spreads don't revert enough to pay costs |
| 2026-11-11 | Known: 6-month momentum, 1-day reversal (cross-sectional, long-short) | as above | Momentum +12.8%/yr research but holdout -47%; reversal holdout +107% but 57% years up | FAIL | Momentum crashed in the holdout; reversal fails years-up and family |
| 2026-11-11 | Intraday search, 9 families (ORB, gap, first move, VWAP, am/pm, time of day, prev-day high/low, late move, 30-min burst), 2,824 variants | 91 stocks, 5-min SIP, May-10 Nov 2026, 5 bp a side; holdout = last 40 days | 105 pass gates 1-4; only 49 of them profitable in the holdout (median -1.0%); 0 pass all | FAIL (all) | Research-period winners were luck. Best: "first 5 min up 0.5% on 2.5x volume, follow" +60% research, -13.7% holdout |
| 2026-11-11 | Live US ORB rule inside the same search | as above | +37% research over 242 trades, holdout -3.4% | FAIL | Fails family, luck bar, holdout, Monte Carlo; matches live -$20 |
| 2026-11-11 | ORB "no trade" days check | SIP 9:30 bars, 91 stocks | Agent's rvol matched SIP recount exactly | OK | 11 Nov had 0 stocks at 2.5x; not a bug |
| 2026-11-10 | 23,085 daily variants, 20 families, 19 ETFs | Yahoo daily 2001-2026 | 5,410 pass rules 1-6, 0 pass the luck bar | FAIL (all) | Best is IBS on QQQ/XLK, which holds in holdout |
| 2026-11-10 | Bad-day prediction (logistic, boosted trees, VIX, events) | Daily, 25 years | No better than guessing | FAIL | Bad days are not predictable from daily data |
| 2026-11-10 | IBS + event-day skips (FOMC, jobs, opex, turn of month) | 25 years | Skip-jobs +$74, weak | WATCH | Too small to trust |
| 2026-11-10 | IBS threshold sensitivity 0.10-0.50, Monte Carlo, walk-forward | 25 years | All thresholds positive; breaks at 5x costs | PASS | Robust |
| 2026-11-09 | IBS low close QQQ: buy close in bottom 20% of range, sell next close | 2001-2026 | +10.1%/yr, 19% time in market, -17% max fall, 19/24 years up | PASS (live) | The book runs it on $1,000 |
| 2026-11-09 | Trend switch, momentum rotation, RSI(2), turn of month, VIX spike | 2001-2026 | Pass but only match buy-and-hold | WATCH | No extra return |
| 2026-11-09 | Overnight hold | 2001-2026 | Loses after costs | FAIL | Costs |
| 2026-11-09 | Options on ORB signals | May-Oct 2026, live | Profit only from far-OTM lottery wins | LIVE-FAIL | Paused 9 Nov |
| 2026-11-09 | Desk roles LUNA (mood), ORION (news veto) | 24 live cards, 2,490 backtest trades | LUNA never objected; ORION veto inconsistent | FAIL | LUNA removed, ORION advisory |
| 2026-11-06 | 0DTE bear call spreads SPY/QQQ, 0.6% OTM, $2 wide, 11:00-15:50 | May-Oct 2026 | +$1,207 on 148, both halves, 5/6 months, -$305 max fall | PASS (live) | Only 6 months; one bad day can lose ~$340 |
| 2026-11-05 | News exits on ORB trades | 210 trades | All variants worse | FAIL | Sells winners as often as losers |
| 2026-11-05 | Skip ORB on FOMC/CPI/jobs days | 210 trades | +$75 -> +$108, both halves | PASS (live) | Small sample |
| 2026-11-04 | Sell-high / rebuy-low (user's idea) | 153 trades + live | +$28 vs +$87 for the plain rule | FAIL | Cuts trend days |
| 2026-11-04 | No ORB entries after 10:00 | 210 trades | Late entries lost in both halves | PASS (live) | |
| 2026-11-03 | ORB stocks picked by IEX vs SIP volume | May-Oct 2026 | IEX +$40, SIP +$99, 29% overlap | Fixed | Rank from delayed SIP at 9:50 |
| 2026-11-03 | US ORB (5-min range, rvol 2.5, far stop, flat 15:45) | 2 years backtest, 12 live trades | Backtest +$75 in 6 months; live -$20.27 | WATCH | Thin edge (~$0.80/day) |
| 2026-11-03 | TimesFM filter on US ORB | May-Oct 2026 | Split reversed vs India | FAIL | |

## India

| Date | Idea | Data | Result | Verdict | Why |
|---|---|---|---|---|---|
| 2026-11-11 | QuantRank, Kalman pairs, momentum/reversal baselines (74 variants) | 51 NIFTY 50 stocks daily 2007-2026, 12 bp a side | Long-only looked +15.8%/yr but only +1.8%/yr over holding all stocks; pairs -0.7%/yr; 0 pass | FAIL (all) | Delivery costs; most "profit" was just holding stocks |
| 2026-11-11 | Intraday search, same 9 families, 2,772 variants | 51 NIFTY 50 stocks, 5-min, May-9 Nov 2026, 0.18% round trip | 3 pass gates 1-3, 0 survive double costs | FAIL (all) | Costs are larger than any intraday edge found |
| 2026-11-10 | 64,395 daily variants on NIFTY, BANKNIFTY, NIFTY 50 stocks | Yahoo daily 2007-2026 | Best (gap-down buys TRENT, BAJFINANCE) fail holdout | FAIL (all) | Fitted to the past |
| 2026-11-10 | India ORB Monte Carlo, walk-forward, sensitivity | May-Oct 2026 | Fails all three | FAIL | |
| 2026-11-10 | Earlier exit than 15:15 | May-Oct 2026 | Not better | FAIL | User kept 15:15 |
| 2026-11-03 | Gap fade, VWAP-stretch fade, afternoon trend, NR7, prior-day breakout | NIFTY 50, May-Oct 2026 | -288 to -2,117 | FAIL | Costs and no edge |
| 2026-11-03 | Gap-and-go | NIFTY 50, May-Oct 2026 | About flat | WATCH (live, Rs 10k) | |
| 2026-11-03 | TimesFM skip filter on India ORB | May-Oct 2026 | +Rs 223 but 2nd half -239, Aug-Oct negative | FAIL | Running as tracking only |
| 2026-11-03 | India ORB plain | May-Oct 2026 | -Rs 574, 1/6 months up | FAIL | Live -Rs 13 total |
| 2026-11-03 | NIFTY ORB / burst options (Black-Scholes priced) | May-Oct 2026 | Profit only from the Sep-Oct trend | FAIL | Upstox option history is paid |
| 2026-11-02 | Opening burst: first bar >=1% on >=3x volume, with NIFTY, stop far side | 188 stocks, 2 years | +18.7% / +27.4% at 3 bp; -3.5% / +19% at 10 bp | WATCH (live) | Very sensitive to slippage |

## Crypto

| Date | Idea | Data | Result | Verdict | Why |
|---|---|---|---|---|---|
| 2026-11-03 | 60-day rule BTC/ETH ($20 book) | Live from 3 Nov | -$0.77, in cash since 6 Nov | WATCH | |
| 2026-11-03 | Trend, breakout, dip-in-uptrend (6 strategies) | Alpaca hourly 2 years | Breakout lost 150-220% of the book | FAIL | Fees |

## Polymarket

| Date | Idea | Data | Result | Verdict | Why |
|---|---|---|---|---|---|
| 2026-11-11 | Full re-test: 2,220 rules (price band x hours before end/game start x type x recent move), only facts known at bet time | 30,591 resolved markets, 15 Aug-8 Nov 2026, any size; 1c cost (2c gate); holdout = ending after 18 Oct | 0 pass strict; 4 pass the softer luck bar. Best: buy the 50-60c side 168 h before the end: 3,356 bets, 59% won, +11.5%/bet, 83% of 2-week blocks up, holdout +44.9 on 469 bets | WATCH | Prices are hourly last/mid, not the real ask; near-50c sports books may be wide, so the 1c cost may be far too low. Needs a shadow test that records the live ask before any money |
| 2026-11-11 | Same 50-60c rule, split by how actively the market traded in the 24 h before the bet | as above | Price exactly 0.500 (placeholder): 49% won, -3.6%/bet. Active markets (8+ distinct prices, 3c+ range): 703 bets, -0.3%/bet. First live shadow scan: 8 picks, real ask a median 44c above the shown price (e.g. shown 50c, book 1c/99c) | LIKELY PHANTOM | The edge lives where nobody can trade at the shown price. Shadow test (`tools/poly_shadow.py loop`, no bets, `research/poly_shadow.jsonl`) running from 11 Nov to confirm with real asks |
| 2026-11-11 | The live underdog rule (20-30c, 168 h before the end) on the full sample | as above | 1,020 bets, 24% won, -8.2%/bet, 17% of blocks up, holdout -58.7 | FAIL | Confirms the live loss; the old +48% came from testing only big markets |
| 2026-11-11 | Buy favourites 70-97c | Volume-selected sample | Break-even at best; matches -6% to -13%/bet | FAIL | |
| 2026-11-11 | Underdog 20-30c a week before the end | Backtest +48%/bet; live 6 won / 35 | Live -55% of $50 | LIVE-FAIL | Backtest saw only markets over ~$98k; live bets were small tennis. Paused; full-sample re-test running |
| 2026-11-03 | LLM fair-value bets | Live | No edge (e.g. priced 4% vs market 37% without data) | FAIL | |

## Template

| Date | Idea | Data | Result | Verdict | Why |
|---|---|---|---|---|---|
| YYYY-MM-DD | one line: the exact rule | market, dates, costs | the numbers that decided it | PASS / WATCH / FAIL / LIVE-FAIL | the gate it failed, or the lesson |
