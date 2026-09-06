"""API Gateway — shared rate limiting.

Two separate concerns, deliberately kept apart:

  * Per source address, to slow an attacker working through a password list.
  * Per username, so a distributed attempt against ONE account is still
    stopped even though each request comes from a different address.

Both are needed. An address limit alone falls to a botnet; a username limit
alone lets someone spray one password across every account.

In-process and per-worker: with `--workers 4` an attacker gets four times the
stated budget. That is a real limitation, and the right fix is a shared store
(Redis) if this is ever exposed to the internet. It is still the difference
between thousands of guesses a minute and a handful.
"""

import logging
import time
from collections import defaultdict
from typing import Optional

logger = logging.getLogger("api_gateway.ratelimit")


class SlidingWindowLimiter:
    """Allow `limit` events per `window` seconds, per key."""

    def __init__(self, limit: int, window: int, name: str = "limiter"):
        self.limit = limit
        self.window = window
        self.name = name
        self._buckets: dict[str, list[float]] = defaultdict(list)

    def _prune(self, key: str, now: float) -> list[float]:
        bucket = self._buckets[key]
        bucket[:] = [t for t in bucket if now - t < self.window]
        # Keep the table from growing without bound as keys age out.
        if not bucket:
            self._buckets.pop(key, None)
            return []
        return bucket

    def allow(self, key: str) -> bool:
        """Record an attempt. False when the caller is over budget."""
        now = time.monotonic()
        bucket = self._prune(key, now)
        if len(bucket) >= self.limit:
            logger.warning(f"{self.name}: over limit for '{key}'")
            return False
        self._buckets[key].append(now)
        return True

    def retry_after(self, key: str) -> int:
        """Seconds until the oldest attempt in the window ages out."""
        bucket = self._buckets.get(key) or []
        if not bucket:
            return 0
        return max(1, int(self.window - (time.monotonic() - bucket[0])))

    def reset(self, key: str) -> None:
        """Clear a key. Called on a SUCCESSFUL login so a legitimate user who
        mistyped twice is not still counting against the limit."""
        self._buckets.pop(key, None)
