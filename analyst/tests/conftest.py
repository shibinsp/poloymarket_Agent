from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

import pytest

from moneyai_analyst.schema import SourceItem
from moneyai_analyst.universe import Universe

IST = ZoneInfo("Asia/Kolkata")
FIXTURES = Path(__file__).parent / "fixtures"


@pytest.fixture
def universe() -> Universe:
    return Universe.from_csv_text((FIXTURES / "nifty_sample.csv").read_text())


@pytest.fixture
def now() -> datetime:
    return datetime(2026, 11, 12, 9, 0, tzinfo=IST)


def make_item(item_id: str, title: str, symbols=("RELIANCE",), kind="news", published_at=None, body="") -> SourceItem:
    return SourceItem(
        id=item_id, kind=kind, source="test", title=title, body=body,
        published_at=published_at or datetime(2026, 11, 12, 8, 30, tzinfo=IST), symbols=tuple(symbols),
    )
