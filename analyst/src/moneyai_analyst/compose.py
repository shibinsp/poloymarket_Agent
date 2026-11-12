"""Compose and validate the published context (the restrict-only 'portfolio' agent).

Deterministic code, not a model: the architecture doc allows a model here only
if golden-set tests show it helps, and composing from validated parts is what
makes every restriction auditable."""

from __future__ import annotations

from datetime import datetime, timedelta

from . import PROMPT_VERSION, SCHEMA_VERSION
from .config import ComposeConfig
from .forecast import vol_multiplier
from .regime import bias_multiplier, regime_bias
from .schema import AnalystContext, Classification, EventType, Reason, Regime, SourceItem, VolBand
from .universe import Universe

# Event types that may *exclude* a stock when severe. Macro news is market-wide
# and handled by the regime bias instead.
_EXCLUDING = {
    EventType.RESULTS, EventType.GUIDANCE, EventType.MERGER, EventType.REGULATORY,
    EventType.MANAGEMENT_CHANGE, EventType.BLOCK_DEAL, EventType.PLEDGE,
    EventType.CORPORATE_ACTION, EventType.RUMOUR, EventType.UNKNOWN, EventType.OTHER,
}


class InvalidContext(ValueError):
    pass


def compose(
    run_id: str,
    now: datetime,
    universe: Universe,
    items: dict[str, SourceItem],
    classifications: list[Classification],
    regime: Regime,
    vol_bands: list[VolBand],
    cfg: ComposeConfig,
    model: str,
    unusual_volume: dict[str, tuple[float, str]] | None = None,
) -> AnalystContext:
    reasons: list[Reason] = []
    exclude: dict[str, list[str]] = {}
    watch_score: dict[str, float] = {}

    for c in classifications:
        for sym in c.symbols:
            if c.severity >= cfg.exclude_severity and c.event_type in _EXCLUDING:
                exclude.setdefault(sym, []).append(c.item_id)
            elif c.severity >= cfg.watch_severity:
                watch_score[sym] = max(watch_score.get(sym, 0.0), c.severity + abs(c.sentiment))

    for sym, (ratio, source_id) in sorted((unusual_volume or {}).items()):
        if sym not in exclude:
            watch_score[sym] = max(watch_score.get(sym, 0.0), 1.0 + min(ratio, 5.0) / 5)
            reasons.append(Reason(text=f"Watch {sym}: last session volume {ratio:.1f}x its 20-day average", source_ids=[source_id]))

    for sym, ids in sorted(exclude.items()):
        titles = "; ".join(items[i].title[:120] for i in ids[:2])
        reasons.append(Reason(text=f"Exclude {sym}: {titles}", source_ids=ids))

    bias, triggers = regime_bias(regime, cfg)
    if triggers and regime.source_ids:
        reasons.append(Reason(text=f"Bias {bias.value}: " + "; ".join(triggers), source_ids=list(regime.source_ids)))

    per_symbol: dict[str, float] = {}
    for band in vol_bands:
        m = vol_multiplier(band, cfg)
        if m < 1.0 and band.symbol not in exclude:
            per_symbol[band.symbol] = m
            reasons.append(
                Reason(
                    text=f"Size {band.symbol} x{m}: forecast vol {band.ratio}x its 1y baseline "
                    f"(next-day band {band.low}-{band.high})",
                    source_ids=[band.source_id],
                )
            )

    watchlist = [s for s, _ in sorted(watch_score.items(), key=lambda kv: (-kv[1], kv[0])) if s not in exclude]
    ctx = AnalystContext(
        run_id=run_id,
        published_at=now,
        expires_at=now + timedelta(minutes=cfg.context_ttl_minutes),
        watchlist=watchlist[: cfg.max_watchlist],
        bias=bias,
        exclude=sorted(exclude),
        size_multiplier=bias_multiplier(bias, cfg),
        symbol_size_multipliers=per_symbol,
        reasons=reasons,
        schema_version=SCHEMA_VERSION,
        prompt_version=PROMPT_VERSION,
        model=model,
    )
    known = set(items) | set(regime.source_ids) | {b.source_id for b in vol_bands}
    known |= {sid for _, sid in (unusual_volume or {}).values()}
    validate_context(ctx, universe, known)
    return ctx


def validate_context(ctx: AnalystContext, universe: Universe, known_source_ids: set[str]) -> None:
    """Final gate before publication. Any failure means nothing is published."""
    named = set(ctx.watchlist) | set(ctx.exclude) | set(ctx.symbol_size_multipliers)
    outside = named - universe.symbols
    if outside:
        raise InvalidContext(f"symbols outside the universe: {sorted(outside)}")
    for r in ctx.reasons:
        missing = set(r.source_ids) - known_source_ids
        if missing:
            raise InvalidContext(f"reason cites unknown sources: {sorted(missing)}")
    if not 0.0 <= ctx.size_multiplier <= 1.0:
        raise InvalidContext("size_multiplier outside [0, 1]")
    if any(not 0.0 <= m <= 1.0 for m in ctx.symbol_size_multipliers.values()):
        raise InvalidContext("per-symbol multiplier outside [0, 1]")
