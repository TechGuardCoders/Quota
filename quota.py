"""Quota: per-tenant token/request budgets with scheduled reset.

Quota(limit=100000, window_s=86400) tracks consumption per tenant over a
sliding-fixed window. At 80% of the limit callers get a warning flag; over
the limit, allow() returns a structured denial (never raises, never blocks)
including retry_after seconds until the window resets.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Callable, Dict


@dataclass
class Denial:
    """Structured rejection returned when a tenant is over budget."""

    tenant: str
    requested: int
    remaining: int
    retry_after: float
    reason: str = "quota_exceeded"

    def __bool__(self) -> bool:
        return False


@dataclass
class _Window:
    started_at: float
    used: int = 0


class Quota:
    """Per-tenant budget with scheduled reset.

    allow(tenant, tokens) -> bool
        True if the request fits in the tenant's remaining budget (and
        consumes it). Over the limit, returns a Denial (falsy) with
        retry_after seconds — never raises, never sleeps.
    """

    def __init__(
        self,
        limit: int,
        window_s: float,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if limit <= 0:
            raise ValueError("limit must be positive")
        if window_s <= 0:
            raise ValueError("window_s must be positive")
        self.limit = limit
        self.window_s = window_s
        self._clock = clock
        self._windows: Dict[str, _Window] = {}

    # -- internals ---------------------------------------------------------

    def _window_for(self, tenant: str) -> _Window:
        now = self._clock()
        win = self._windows.get(tenant)
        if win is None or now - win.started_at >= self.window_s:
            win = _Window(started_at=now)
            self._windows[tenant] = win
        return win

    def _retry_after(self, win: _Window) -> float:
        elapsed = self._clock() - win.started_at
        return max(0.0, self.window_s - elapsed)

    # -- public API --------------------------------------------------------

    def allow(self, tenant: str, tokens: int) -> bool:
        """Try to consume `tokens` for `tenant`.

        Returns True on success; a falsy Denial over budget. A request
        larger than the whole limit is rejected immediately (it can never
        fit in any window).
        """
        if tokens < 0:
            raise ValueError("tokens must be non-negative")
        if tokens > self.limit:
            return Denial(
                tenant=tenant,
                requested=tokens,
                remaining=self.remaining(tenant),
                retry_after=self._retry_after(self._window_for(tenant)),
                reason="request_exceeds_total_limit",
            )

        win = self._window_for(tenant)
        if win.used + tokens > self.limit:
            return Denial(
                tenant=tenant,
                requested=tokens,
                remaining=self.limit - win.used,
                retry_after=self._retry_after(win),
            )
        win.used += tokens
        return True

    def warn(self, tenant: str) -> bool:
        """True once the tenant has consumed >= 80% of the limit."""
        win = self._windows.get(tenant)
        if win is None:
            return False
        return win.used >= 0.8 * self.limit

    def remaining(self, tenant: str) -> int:
        """Tokens left for the tenant in the current window."""
        win = self._window_for(tenant)
        return max(0, self.limit - win.used)

    def usage(self, tenant: str) -> int:
        """Tokens consumed by the tenant in the current window."""
        return self._window_for(tenant).used
