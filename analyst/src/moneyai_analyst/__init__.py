"""MoneyAI analyst sidecar.

A restrict-only context publisher for the NSE intraday engine. It reads news,
filings and market regime data, and publishes a short-lived context record
(watchlist, bias, exclusions, size multipliers <= 1). It never places orders,
never picks direction and never scales size up.

The four agents of the multi-agent design map onto modules as follows:

* Quant / forecasting agent  -> ``forecast`` (volatility bands) + ``regime``
* Filings RAG agent          -> ``sources.filings`` + ``rag``
* News sentiment agent       -> ``sources.news`` + ``classify``
* Portfolio / risk agent     -> ``compose`` + ``validate`` (restrict-only).
  The real risk gate stays in the Rust engine.
"""

SCHEMA_VERSION = "1.0"
PROMPT_VERSION = "classify-v1"
