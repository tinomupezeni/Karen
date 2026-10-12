"""Hand landmark tracking via MediaPipe Tasks HandLandmarker.

Owns camera-frame -> 21-landmark detection. Returns plain data (no cv2
dependency for callers): a list of Hand dicts with normalized landmarks.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from pathlib import Path
from urllib.request import urlretrieve


@dataclass
class Hand:
    landmarks: list[tuple[float, float, float]]  # 21 x (x, y, z), 0..1
    handedness: str  # "Left" or "Right"


@dataclass
class HandTracker:
    model_path: Path
    num_hands: int = 2
    min_detection_confidence: float = 0.5
    min_presence_confidence: float = 0.5
    min_tracking_confidence: float = 0.5
    _landmarker: object = field(default=None, repr=False)

    def ensure_model(self, url: str) -> Path:
        self.model_path.parent.mkdir(parents=True, exist_ok=True)
        if not self.model_path.exists():
            print(f"[tracker] downloading hand model from {url}")
            urlretrieve(url, self.model_path)
            print(f"[tracker] saved to {self.model_path}")
        return self.model_path

    def start(self) -> None:
        import mediapipe as mp

        base = mp.tasks.BaseOptions(model_asset_path=str(self.model_path))
        options = mp.tasks.vision.HandLandmarkerOptions(
            base_options=base,
            running_mode=mp.tasks.vision.RunningMode.VIDEO,
            num_hands=self.num_hands,
            min_hand_detection_confidence=self.min_detection_confidence,
            min_hand_presence_confidence=self.min_presence_confidence,
            min_tracking_confidence=self.min_tracking_confidence,
        )
        self._landmarker = mp.tasks.vision.HandLandmarker.create_from_options(options)

    def detect(self, bgr_frame) -> list[Hand]:
        import mediapipe as mp

        rgb = bgr_frame[:, :, ::-1].copy()
        mp_image = mp.Image(image_format=mp.ImageFormat.SRGB, data=rgb)
        ts_ms = int(time.monotonic() * 1000)
        result = self._landmarker.detect_for_video(mp_image, ts_ms)
        hands: list[Hand] = []
        for hand_landmarks, handedness in zip(
            result.hand_landmarks, result.handedness, strict=False
        ):
            hands.append(
                Hand(
                    landmarks=[(lm.x, lm.y, lm.z) for lm in hand_landmarks],
                    handedness=handedness[0].category_name or "?",
                )
            )
        return hands

    def close(self) -> None:
        if self._landmarker is not None:
            try:
                self._landmarker.close()
            except Exception:
                pass
            self._landmarker = None
