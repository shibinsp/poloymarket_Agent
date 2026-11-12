-- The crypto paper books: $10 BTC + $10 ETH each, on a 60-day momentum rule.
--   crypto       Alpaca paper orders; the news-and-market check can veto a buy
--                or move it to cash on severe news.
--   crypto_rule  the same rule alone, filled on paper at the same prices, so
--                the news check is measured rather than assumed to help.
CREATE TABLE IF NOT EXISTS crypto_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    book TEXT NOT NULL,
    symbol TEXT NOT NULL,
    -- OPEN, CLOSED
    status TEXT NOT NULL,
    qty TEXT NOT NULL,
    -- dollars paid, fees included
    cost TEXT NOT NULL,
    entry_price TEXT NOT NULL,
    entry_at TEXT NOT NULL,
    entry_reason TEXT NOT NULL,
    client_order_id TEXT,
    exit_client_order_id TEXT,
    exit_price TEXT,
    exit_at TEXT,
    exit_reason TEXT,
    proceeds TEXT,
    pnl TEXT
);
CREATE INDEX IF NOT EXISTS idx_crypto_trades_book ON crypto_trades(book, symbol, status);

-- Every look at the market: the rule's answer, the news read and what was done.
CREATE TABLE IF NOT EXISTS crypto_checks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    day TEXT NOT NULL,
    symbol TEXT NOT NULL,
    -- daily (the rule's decision) or hourly (news watch while holding)
    kind TEXT NOT NULL,
    price TEXT NOT NULL,
    chg_24h TEXT,
    chg_7d TEXT,
    chg_60d TEXT,
    -- HOLD or CASH
    rule_says TEXT NOT NULL,
    -- normal, severe or unknown (the model did not answer)
    news_risk TEXT NOT NULL,
    news_reason TEXT,
    market_view TEXT,
    headlines TEXT,
    action TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_crypto_checks_symbol ON crypto_checks(symbol, kind, day);
