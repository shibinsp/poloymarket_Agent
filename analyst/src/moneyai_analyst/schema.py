"""Data shapes shared by every stage. All model output is validated against these."""

from __future__ import annotations

from datetime import datetime
from enum import Enum
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator


class EventType(str, Enum):
    RESULTS = "results"
    GUIDANCE = "guidance"
    MERGER = "merger"
    REGULATORY = "regulatory"
    MANAGEMENT_CHANGE = "management_change"
    BLOCK_DEAL = "block_deal"
    PLEDGE = "pledge"
    CORPORATE_ACTION = "corporate_action"
    RUMOUR = "rumour"
    MACRO = "macro"
    OTHER = "other"
    UNKNOWN = "unknown"


class Bias(str, Enum):
    NEUTRAL = "neutral"
    CAUTIOUS = "cautious"
    RISK_OFF = "risk_off"


class SourceItem(BaseModel):
    """One piece of evidence: a news headline, a filing, or a market data point."""

    model_config = ConfigDict(frozen=True)

    id: str
    kind: Literal["news", "filing", "market", "calendar"]
    source: str
    title: str
    body: str = ""
    url: str | None = None
    published_at: datetime
    symbols: tuple[str, ...] = ()


class Classification(BaseModel):
    """Output of the classify step for exactly one source item."""

    model_config = ConfigDict(extra="forbid")

    item_id: str
    event_type: EventType
    severity: int = Field(ge=0, le=3)
    sentiment: float = Field(ge=-1.0, le=1.0)
    symbols: list[str] = Field(default_factory=list)
    summary: str = Field(default="", max_length=240)
    evidence_ids: list[str] = Field(min_length=1)
    classifier: str = "unknown"


class Reason(BaseModel):
    model_config = ConfigDict(extra="forbid")

    text: str = Field(max_length=400)
    source_ids: list[str] = Field(min_length=1)


class VolBand(BaseModel):
    """Forecast volatility for one symbol over the next session(s)."""

    model_config = ConfigDict(extra="forbid")

    symbol: str
    forecast_daily_vol: float = Field(ge=0)
    baseline_daily_vol: float = Field(gt=0)
    ratio: float = Field(ge=0)
    low: float
    high: float
    source_id: str


class Regime(BaseModel):
    model_config = ConfigDict(extra="forbid")

    india_vix: float | None = None
    india_vix_change_pct: float | None = None
    nifty_return_pct: float | None = None
    nifty_above_sma20: bool | None = None
    fii_net_cr: float | None = None
    dii_net_cr: float | None = None
    source_ids: list[str] = Field(default_factory=list)


class AnalystContext(BaseModel):
    """The only thing the analyst publishes. Restrict-only by construction.

    * ``size_multiplier`` and every per-symbol multiplier are in [0, 1].
    * ``mode`` is fixed to ``shadow``: consumers must ignore restrictions until the
      value-of-restrictions report justifies enforcement, which requires a code change.
    """

    model_config = ConfigDict(extra="forbid")

    mode: Literal["shadow"] = "shadow"
    run_id: str
    published_at: datetime
    expires_at: datetime
    watchlist: list[str] = Field(default_factory=list, max_length=20)
    bias: Bias = Bias.NEUTRAL
    exclude: list[str] = Field(default_factory=list)
    size_multiplier: float = Field(default=1.0, ge=0.0, le=1.0)
    symbol_size_multipliers: dict[str, float] = Field(default_factory=dict)
    reasons: list[Reason] = Field(default_factory=list)
    schema_version: str
    prompt_version: str
    model: str

    @field_validator("symbol_size_multipliers")
    @classmethod
    def _per_symbol_never_above_one(cls, v: dict[str, float]) -> dict[str, float]:
        for sym, m in v.items():
            if not 0.0 <= m <= 1.0:
                raise ValueError(f"size multiplier for {sym} must be within [0, 1], got {m}")
        return v

    @model_validator(mode="after")
    def _expiry_after_publication(self) -> "AnalystContext":
        if self.expires_at <= self.published_at:
            raise ValueError("expires_at must be after published_at")
        if self.published_at.tzinfo is None or self.expires_at.tzinfo is None:
            raise ValueError("timestamps must be timezone-aware")
        overlap = set(self.watchlist) & set(self.exclude)
        if overlap:
            raise ValueError(f"symbols both watched and excluded: {sorted(overlap)}")
        return self

    def is_active(self, now: datetime) -> bool:
        return self.published_at <= now < self.expires_at
