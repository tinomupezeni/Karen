"""Rule-based gesture classification over 21 MediaPipe landmarks.

Landmark indices (wrist=0, thumb 1-4, index 5-8, middle 9-12, ring 13-16,
pinky 17-20; tip is the last of each group). Image coords: y grows downward.

Stabilizer turns flickery per-frame labels into discrete events: a gesture
must hold for `stable_frames` consecutive frames, then observes
`cooldown_ms` before it may fire again.
"""

from __future__ import annotations

import math
import time
from dataclasses import dataclass, field

FINGER_TIPS = {"thumb": 4, "index": 8, "middle": 12, "ring": 16, "pinky": 20}
FINGER_PIPS = {"thumb": 3, "index": 6, "middle": 10, "ring": 14, "pinky": 18}


def _dist(a: tuple[float, float, float], b: tuple[float, float, float]) -> float:
    return math.hypot(a[0] - b[0], a[1] - b[1])


def palm_scale(lm: list[tuple[float, float, float]]) -> float:
    """Palm size as a distance normalizer (scale-invariant gestures)."""
    return max(_dist(lm[0], lm[9]), _dist(lm[5], lm[17]), 1e-6)


def finger_up(lm: list[tuple[float, float, float]], finger: str, handedness: str) -> bool:
    tip, pip = FINGER_TIPS[finger], FINGER_PIPS[finger]
    if finger == "thumb":
        # Thumb extends sideways: compare x against index MCP, handedness-aware.
        if handedness == "Left":
            return lm[tip][0] > lm[5][0]
        return lm[tip][0] < lm[5][0]
    return lm[tip][1] < lm[pip][1]


def classify(lm: list[tuple[float, float, float]], handedness: str, pinch_threshold: float = 0.05) -> str:
    """Classify one hand. Returns gesture name or 'none'/'unknown'."""
    if len(lm) != 21:
        return "none"
    scale = palm_scale(lm)
    pinch = _dist(lm[4], lm[8]) / scale
    if pinch < pinch_threshold:
        return "pinch"
    states = {f: finger_up(lm, f, handedness) for f in ("thumb", "index", "middle", "ring", "pinky")}
    extended = [f for f, up in states.items() if up]
    if len(extended) == 0:
        return "fist"
    if len(extended) == 5:
        return "open_palm"
    if extended == ["index", "middle"]:
        return "peace"
    if extended == ["thumb"]:
        return "thumbs_up"
    return "unknown"


@dataclass
class Stabilizer:
    stable_frames: int = 5
    cooldown_ms: int = 1200
    _candidate: str = ""
    _count: int = 0
    _last_fire_ms: int = 0

    def update(self, label: str, now_ms: int | None = None) -> str | None:
        """Feed a per-frame label; returns the gesture when it becomes a stable event."""
        now_ms = int(time.monotonic() * 1000) if now_ms is None else now_ms
        if label in ("none", "unknown", ""):
            self._candidate = ""
            self._count = 0
            return None
        if label == self._candidate:
            self._count += 1
        else:
            self._candidate = label
            self._count = 1
        if self._count >= self.stable_frames and now_ms - self._last_fire_ms >= self.cooldown_ms:
            self._last_fire_ms = now_ms
            self._count = 0  # require a fresh hold before refiring
            return label
        return None
