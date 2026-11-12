"""Thin async LLM client: OpenAI-compatible chat completions or Anthropic Messages.

Temperature 0, bounded output, a hard timeout and one retry. Callers treat any
exception as 'no answer' and fall back; nothing here repairs bad output."""

from __future__ import annotations

import asyncio
import json
import re

import httpx

from .config import LlmConfig


class LlmError(RuntimeError):
    pass


class LlmClient:
    def __init__(self, cfg: LlmConfig, api_key: str | None, client: httpx.AsyncClient | None = None):
        if cfg.provider not in {"openai_compatible", "anthropic"}:
            raise LlmError(f"unsupported provider {cfg.provider!r}")
        if not api_key:
            raise LlmError(f"missing API key in ${cfg.api_key_env}")
        self.cfg = cfg
        self.api_key = api_key
        self.client = client or httpx.AsyncClient(timeout=cfg.timeout_s)

    @property
    def model_id(self) -> str:
        return f"{self.cfg.provider}:{self.cfg.model}"

    async def complete(self, system: str, user: str, max_tokens: int) -> str:
        last: Exception | None = None
        for attempt in range(self.cfg.retries + 1):
            try:
                return await asyncio.wait_for(self._call(system, user, max_tokens), self.cfg.timeout_s)
            except Exception as exc:  # timeout, HTTP error, malformed body
                last = exc
                if attempt < self.cfg.retries:
                    await asyncio.sleep(0.5)
        raise LlmError(str(last))

    async def _call(self, system: str, user: str, max_tokens: int) -> str:
        if self.cfg.provider == "anthropic":
            resp = await self.client.post(
                "https://api.anthropic.com/v1/messages",
                headers={"x-api-key": self.api_key, "anthropic-version": "2023-06-01"},
                json={
                    "model": self.cfg.model,
                    "max_tokens": max_tokens,
                    "temperature": self.cfg.temperature,
                    "system": system,
                    "messages": [{"role": "user", "content": user}],
                },
            )
            resp.raise_for_status()
            return _require_text("".join(b.get("text", "") for b in resp.json()["content"]))
        resp = await self.client.post(
            f"{self.cfg.base_url.rstrip('/')}/chat/completions",
            headers={"Authorization": f"Bearer {self.api_key}"},
            json={
                **self.cfg.extra_body,
                "model": self.cfg.model,
                "max_tokens": max_tokens,
                "temperature": self.cfg.temperature,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            },
        )
        resp.raise_for_status()
        return _require_text(resp.json()["choices"][0]["message"].get("content"))

    async def aclose(self) -> None:
        await self.client.aclose()


def _require_text(content) -> str:
    """Reasoning models can spend the whole budget thinking and return no content."""
    if not isinstance(content, str) or not content.strip():
        raise LlmError("model returned no content (budget exhausted or empty reply)")
    return content


_FENCE = re.compile(r"^```(?:json)?\s*|\s*```$", re.MULTILINE)


def extract_json(text: str) -> dict:
    """Accept a bare JSON object, optionally inside a markdown fence. Anything
    else raises; there is no guessing at malformed output."""
    stripped = _FENCE.sub("", text.strip()).strip()
    obj = json.loads(stripped)
    if not isinstance(obj, dict):
        raise ValueError("expected a JSON object")
    return obj
