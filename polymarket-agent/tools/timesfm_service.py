"""Local TimesFM forecast service for the India ORB filter.

Loads Google's TimesFM 3.0 once and answers on 127.0.0.1 only:

    POST /forecast  {"series": [5-minute closes, oldest first], "horizon": n}
    ->              {"end": forecast close n bars ahead, "last": last close}
    GET  /health    {"ok": true, "model": "..."}

The agent asks it before each India opening-range entry and skips the trade
when the forecast for 15:15 is on the wrong side of the signal close. Chosen on
3 Nov 2026 from May-Oct 2026 data: trades it agreed with made +Rs 342 (55),
those it disagreed with lost Rs 2,125 (83), in both halves. Its plain
direction calls were a coin flip (50.3%) and on US trades the split was the
other way round, so this is a test, tracked on a shadow ledger.

Run with the tfm_env next to the agent:
    ..\\tfm_env\\Scripts\\python.exe tools\\timesfm_service.py
"""
import json
import logging
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import numpy as np
from timesfm3 import ModelConfig, TimesFM3Evaluator

HOST, PORT = "127.0.0.1", 8091
CHECKPOINT = "google/timesfm-3.0-pytorch"
MAX_CONTEXT = 512
MAX_HORIZON = 128

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("timesfm_service")

t0 = time.time()
MODEL = TimesFM3Evaluator(ModelConfig(checkpoint_path=CHECKPOINT, per_core_batch_size=8, device="cpu"))
log.info("TimesFM loaded in %.0fs", time.time() - t0)


class Handler(BaseHTTPRequestHandler):
    def _reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            self._reply(200, {"ok": True, "model": CHECKPOINT})
        else:
            self._reply(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/forecast":
            self._reply(404, {"error": "not found"})
            return
        try:
            req = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))))
            series = np.asarray(req["series"], dtype=np.float32)[-MAX_CONTEXT:]
            horizon = int(req["horizon"])
            if len(series) < 32 or not 1 <= horizon <= MAX_HORIZON or not np.all(np.isfinite(series)):
                raise ValueError("need 32+ finite points and a horizon of 1-128")
            out = next(iter(MODEL.predict_batch([series], horizon=horizon, return_quantiles=False,
                                                use_symmetric_averaging=False)))
            self._reply(200, {"end": float(out.forecast[horizon - 1]), "last": float(series[-1])})
        except Exception as e:  # noqa: BLE001 - every failure is the caller's to handle
            log.warning("bad forecast request: %s", e)
            self._reply(400, {"error": str(e)})

    def log_message(self, fmt, *args):
        log.info("%s %s", self.address_string(), fmt % args)


if __name__ == "__main__":
    log.info("listening on http://%s:%d", HOST, PORT)
    HTTPServer((HOST, PORT), Handler).serve_forever()
