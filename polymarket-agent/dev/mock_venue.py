"""A fake Alpaca and a fake model, so the agent can be run end to end with no
credentials and no network.

Serves both on one port: the Alpaca trading and market-data endpoints a cycle
touches, and an OpenAI-compatible /v1/chat/completions that always returns a
tradeable bullish view. Enough for the agent to discover an instrument, take a
view, size it, place an order, fill it, record the fill, reconcile, and stop at
the position limit.

    python3 dev/mock_venue.py          # then point a config at 127.0.0.1:18800

Two things here are load-bearing and were each learned by getting them wrong:

* **ThreadingHTTPServer, not HTTPServer.** The agent opens concurrent
  keep-alive connections; a single-threaded server never accepts the second
  one and the cycle hangs silently after "Parallel evaluations complete".
* **Positions accumulate.** Replacing the position on each fill makes the
  venue report less than the ledger after the second buy, which the agent
  correctly reads as drift and halts on — a mock bug that looks exactly like
  an agent bug.
"""
import json, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

CASH, EQUITY = "4000.00", "4200.00"
ORDERS = []          # everything the agent submitted
POSITIONS = []       # grows once an order fills
FILL_AFTER = 1       # fill the Nth order onward, so reconciliation has work

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass

    def _send(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        p = urlparse(self.path).path
        if p == "/v2/account":
            return self._send({"id": "acct-1", "currency": "USD", "status": "ACTIVE",
                               "cash": CASH, "equity": EQUITY, "buying_power": "8000.00",
                               "non_marginable_buying_power": CASH})
        if p == "/v2/assets":
            return self._send([{"symbol": "BTC/USD", "class": "crypto", "exchange": "CRYPTO",
                                "name": "Bitcoin / US Dollar", "status": "active",
                                "tradable": True, "fractionable": True,
                                "min_order_size": "0.000026",
                                "min_trade_increment": "0.000000001",
                                "price_increment": "1"}])
        if p == "/v1beta3/crypto/us/latest/orderbooks":
            return self._send({"orderbooks": {"BTC/USD": {
                "t": "2026-09-21T10:00:00Z",
                "b": [{"p": 60000.0, "s": 5.0}], "a": [{"p": 60010.0, "s": 5.0}]}}})
        if p == "/v1beta3/crypto/us/bars":
            bars = []
            for i in range(40):
                base = 60000.0 + i * 25.0
                bars.append({"t": "2026-09-%02dT10:00:00Z" % ((i % 28) + 1),
                             "o": base, "h": base + 300.0, "l": base - 300.0,
                             "c": base + 50.0, "v": 10})
            return self._send({"bars": {"BTC/USD": bars}})
        if p == "/v2/positions":
            return self._send(POSITIONS)
        if p == "/v2/orders":
            return self._send([])
        if p.startswith("/v2/orders/"):
            oid = p.rsplit("/", 1)[1]
            for o in ORDERS:
                if o["id"] == oid or o["client_order_id"] == oid:
                    return self._send(o)
            return self._send({"message": "order not found"}, 404)
        return self._send({"message": f"unmocked {p}"}, 404)

    def do_POST(self):
        p = urlparse(self.path).path
        n = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")

        if p == "/v1/chat/completions":
            view = {"direction": "long", "p_up": 0.72, "expected_return_pct": 0.18,
                    "horizon_hours": 24, "confidence": 0.8,
                    "invalidation_price": 57000, "target_price": 64000,
                    "reasoning_summary": "mock", "key_factors": ["mock"],
                    "data_quality": "high"}
            return self._send({"choices": [{"message": {"content": json.dumps(view)}}],
                               "usage": {"prompt_tokens": 100, "completion_tokens": 50}})

        if p == "/v2/orders":
            i = len(ORDERS) + 1
            filled = i >= FILL_AFTER
            qty = body.get("qty", "0.001")
            order = {"id": f"venue-order-{i}",
                     "client_order_id": body.get("client_order_id", f"cid-{i}"),
                     "symbol": body.get("symbol", "BTC/USD"),
                     "status": "filled" if filled else "accepted",
                     "qty": qty, "filled_qty": qty if filled else "0",
                     "side": body.get("side", "buy"), "type": body.get("type", "limit"),
                     "time_in_force": body.get("time_in_force", "gtc")}
            if filled:
                order["filled_avg_price"] = "60005.00"
                # Accumulate. Replacing made the venue report less than the
                # ledger after the second buy, which the agent correctly read
                # as drift and halted on — a mock bug that looked like one.
                held = sum(float(p["qty"]) for p in POSITIONS)
                POSITIONS.clear()
                POSITIONS.append({"symbol": "BTC/USD",
                                  "qty": f"{held + float(qty):.9f}",
                                  "avg_entry_price": "60005.00", "side": "long",
                                  "asset_class": "crypto"})
            ORDERS.append(order)
            return self._send(order)
        return self._send({"message": f"unmocked {p}"}, 404)

    def do_DELETE(self):
        return self._send([])

# Threaded: the agent opens concurrent keep-alive connections, and a
# single-threaded server blocks the second one forever.
srv = ThreadingHTTPServer(("127.0.0.1", 18800), H)
srv.serve_forever()
