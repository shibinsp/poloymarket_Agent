"""Configuration. Thresholds can tighten restrictions; they can never make the
analyst loosen anything, because the schema caps every multiplier at 1."""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class LlmConfig:
    # "openai_compatible" (NVIDIA NIM, Mistral, local vLLM/Ollama) | "anthropic" | "none"
    provider: str = "none"
    base_url: str = "https://integrate.api.nvidia.com/v1"
    model: str = "meta/llama-3.3-70b-instruct"
    api_key_env: str = "LLM_API_KEY"
    temperature: float = 0.0
    max_tokens_classify: int = 300
    timeout_s: float = 20.0
    retries: int = 1
    body_chars: int = 1500
    max_items_per_run: int = 30
    concurrency: int = 4
    # Extra request fields for OpenAI-compatible servers, e.g. to switch off a
    # reasoning model's thinking: {"chat_template_kwargs": {"enable_thinking": false}}
    extra_body: dict = field(default_factory=dict)


@dataclass
class ComposeConfig:
    exclude_severity: int = 2
    watch_severity: int = 1
    max_watchlist: int = 12
    context_ttl_minutes: int = 20
    # Regime thresholds
    vix_cautious: float = 17.0
    vix_risk_off: float = 22.0
    vix_jump_risk_off_pct: float = 15.0
    nifty_drop_cautious_pct: float = -1.0
    nifty_drop_risk_off_pct: float = -2.0
    fii_sell_cautious_cr: float = -3000.0
    multiplier_neutral: float = 1.0
    multiplier_cautious: float = 0.75
    multiplier_risk_off: float = 0.5
    # Volatility bands -> per-symbol size cut
    vol_ratio_cut1: float = 1.5
    vol_ratio_cut2: float = 2.0
    vol_cut1_multiplier: float = 0.75
    vol_cut2_multiplier: float = 0.5


@dataclass
class SourcesConfig:
    rss_feeds: list[str] = field(default_factory=list)
    bse_announcements: bool = False
    nse_fii_dii: bool = False  # opt-in: check NSE website terms before enabling
    market_data: bool = True  # yfinance: India VIX, Nifty, daily OHLCV
    events_calendar: str | None = None  # CSV: symbol,date,event_type,description
    universe_csv: str | None = None  # Nifty 100 constituents CSV (Symbol, Company Name)
    universe_url: str = "https://niftyindices.com/IndexConstituent/ind_nifty100list.csv"
    http_timeout_s: float = 10.0
    # Headlines matching any of these (case-insensitive regex) carry no information.
    ignore_title_patterns: list[str] = field(default_factory=lambda: [r"share price live updates?"])


@dataclass
class Config:
    state_dir: str = "state"
    llm: LlmConfig = field(default_factory=LlmConfig)
    compose: ComposeConfig = field(default_factory=ComposeConfig)
    sources: SourcesConfig = field(default_factory=SourcesConfig)
    refresh_minutes: int = 15
    run_budget_s: float = 120.0

    @property
    def state_path(self) -> Path:
        return Path(self.state_dir)

    def api_key(self) -> str | None:
        return os.environ.get(self.llm.api_key_env) or None


def _merge(dc, values: dict):
    for k, v in values.items():
        if not hasattr(dc, k):
            raise ValueError(f"unknown config key: {k}")
        cur = getattr(dc, k)
        if hasattr(cur, "__dataclass_fields__"):
            _merge(cur, v)
        else:
            setattr(dc, k, v)
    return dc


def load_config(path: str | os.PathLike | None) -> Config:
    cfg = Config()
    if path:
        with open(path, "rb") as fh:
            _merge(cfg, tomllib.load(fh))
    return cfg


def load_env_file(path: str | os.PathLike) -> None:
    """Minimal KEY=VALUE loader so the sidecar can share polymarket-agent/.env.
    Existing environment variables win."""
    p = Path(path)
    if not p.exists():
        return
    for line in p.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        k, v = k.strip(), v.strip().strip('"').strip("'")
        if k and v and k not in os.environ:
            os.environ[k] = v
