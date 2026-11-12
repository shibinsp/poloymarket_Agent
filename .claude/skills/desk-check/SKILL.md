---
name: desk-check
description: Health and money check of every paper book in the trading agent (India ORB, burst, gap-and-go; US ORB, spreads, IBS; crypto; Polymarket; the Alpaca account), with open positions and their P&L, progress to the daily target, and anything broken (agent halted or in standby, stale cycles, Upstox feed down in market hours, TimesFM helper or stay-awake helper down, halts from the circuit breaker). Use when the user asks "how are we doing", "report", "status", "any open trades", "is everything working", "check the agent", "what happened today", or for the hourly report.
---

# Desk check

Read-only. Never place, change or cancel orders, edit code or config, or restart
anything from this skill. Never print keys or tokens.

## Steps

1. From `C:\Users\gowth\poloymarket_Agent\polymarket-agent` run:
   `python -I tools/desk_check.py .env`
   It reads the agent on 127.0.0.1:8088 and the Alpaca paper account and prints
   one block. (If that file is missing, the same script lives in the session
   scratchpad as `hourly_report.py`.)

2. Flag problems first, plainly:
   - agent HALTED or standby: read `/api/health` `halt.detail` for the reason
     (for example the circuit breaker's daily trade count) and say what it
     blocks; the fix needs the user's yes, and a restart only outside market
     hours;
   - last cycle older than 5 minutes;
   - Upstox feed disconnected between 09:15 and 15:30 IST (outside those hours
     it is normal); the token expires 03:30 IST every day, and the user must
     paste a new one;
   - TimesFM helper (:8091) down: the India ORB trades without its filter;
   - stay-awake helper down: the laptop and screen may sleep;
   - any API unreachable.

3. Then the books, one short table: today and total for each book, and every
   open position with entry, current price, stop and P&L. For open India trades
   not in the agent's live feed the script prices them from Yahoo; say so.

4. Close with the Alpaca account change today and the Rs 500/day target line.
   Keep it short. Plain words, no jargon the user hasn't seen before.

## Notes

- The US session in IST is 20:00-02:30 in winter (19:00-01:30 in summer). The
  spread book enters at 11:00 New York and buys back at 15:50; IBS decides
  about 15:55.
- Polymarket new bets are paused (`polymarket_new_bets = false`) pending a full
  re-test; open bets still settle.
