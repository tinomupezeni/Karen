"""Friday gesture sidecar: camera -> MediaPipe landmarks -> stable gesture
events -> Linux actions. Decoupled from the Friday Tauri backend by design.

Event protocol (one JSON object per line on stdout, UTF-8 NDJSON):
  {"ts": 173..., "type": "gesture", "gesture": "fist", "hand": "Right",
   "action": "play_pause", "action_ok": true, "action_msg": "ok"}
  {"ts": ..., "type": "status", "status": "started|stopped|no_camera", ...}

A future Friday backend consumes stdout (Tauri sidecar) or --post-url.
`--dry-run` detects and prints without touching the system.

Usage:
  uv run --with ... (or .venv) python main.py [--dry-run] [--no-preview]
         [--max-frames N] [--post-url http://127.0.0.1:PORT/gesture]
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent

from actions import ActionRunner  # noqa: E402
from gestures import Stabilizer, classify  # noqa: E402
from hand_tracker import HandTracker  # noqa: E402

SKELETON = (
    (0, 1), (1, 2), (2, 3), (3, 4),
    (0, 5), (5, 6), (6, 7), (7, 8),
    (5, 9), (9, 10), (10, 11), (11, 12),
    (9, 13), (13, 14), (14, 15), (15, 16),
    (13, 17), (17, 18), (18, 19), (19, 20), (0, 17),
)


def emit(event: dict, post_url: str = "") -> None:
    line = json.dumps(event)
    print(line, flush=True)
    if post_url:
        try:
            req = urllib.request.Request(
                post_url, data=line.encode(), headers={"Content-Type": "application/json"}
            )
            urllib.request.urlopen(req, timeout=2).read()
        except Exception as exc:  # best effort: never break the loop
            print(json.dumps({"type": "warn", "warn": f"post failed: {exc}"}), flush=True)


def draw_overlay(cv2, frame, hands, last_event: str) -> None:
    h, w = frame.shape[:2]
    for hand in hands:
        pts = [(int(x * w), int(y * h)) for x, y, _ in hand.landmarks]
        for a, b in SKELETON:
            cv2.line(frame, pts[a], pts[b], (0, 255, 0), 2)
        for x, y in pts:
            cv2.circle(frame, (x, y), 3, (0, 0, 255), -1)
    cv2.putText(frame, last_event, (10, 30),
                cv2.FONT_HERSHEY_SIMPLEX, 0.8, (255, 255, 255), 2)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default=str(HERE / "config.json"))
    ap.add_argument("--dry-run", action="store_true", help="detect only, no system actions")
    ap.add_argument("--no-preview", action="store_true")
    ap.add_argument("--max-frames", type=int, default=0, help="exit after N frames (0 = run forever)")
    ap.add_argument("--post-url", default="")
    args = ap.parse_args()

    import cv2

    cfg = json.loads(Path(args.config).read_text())
    post_url = args.post_url or cfg.get("events", {}).get("post_url", "")

    tracker = HandTracker(
        model_path=HERE / cfg["tracker"]["model_file"],
        num_hands=cfg["tracker"]["num_hands"],
        min_detection_confidence=cfg["tracker"]["min_detection_confidence"],
        min_presence_confidence=cfg["tracker"]["min_presence_confidence"],
        min_tracking_confidence=cfg["tracker"]["min_tracking_confidence"],
    )
    tracker.ensure_model(cfg["tracker"]["model_url"])
    tracker.start()

    runner = ActionRunner(dry_run=args.dry_run)
    stabilizers: dict[str, Stabilizer] = {}
    gcfg = cfg["gestures"]

    cap = cv2.VideoCapture(cfg["camera"]["index"])
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg["camera"]["width"])
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg["camera"]["height"])
    if not cap.isOpened():
        emit({"type": "status", "status": "no_camera", "index": cfg["camera"]["index"]}, post_url)
        return 1
    emit({"type": "status", "status": "started", "dry_run": args.dry_run}, post_url)

    last_event = "show a hand (q quits)"
    frames = 0
    try:
        while True:
            ok, frame = cap.read()
            if not ok:
                emit({"type": "warn", "warn": "frame read failed"}, post_url)
                time.sleep(0.1)
                continue
            frame = frame[:, ::-1, :]  # mirror so it feels like a mirror
            hands = tracker.detect(frame)
            for hand in hands:
                label = classify(hand.landmarks, hand.handedness, gcfg["pinch_threshold"])
                stab = stabilizers.setdefault(
                    hand.handedness,
                    Stabilizer(gcfg["stable_frames"], gcfg["cooldown_ms"]),
                )
                fired = stab.update(label)
                if fired:
                    action = cfg["bindings"].get(fired, "")
                    ok_act, msg = runner.run(action) if action else (False, "unbound")
                    last_event = f"{hand.handedness}: {fired} -> {action or 'unbound'}"
                    emit({"ts": time.time(), "type": "gesture", "gesture": fired,
                          "hand": hand.handedness, "action": action,
                          "action_ok": ok_act, "action_msg": msg}, post_url)
            if not args.no_preview:
                draw_overlay(cv2, frame, hands, last_event)
                cv2.imshow("friday-gesture (q quits)", frame)
                if cv2.waitKey(1) & 0xFF == ord("q"):
                    break
            frames += 1
            if args.max_frames and frames >= args.max_frames:
                break
    except KeyboardInterrupt:
        pass
    finally:
        cap.release()
        tracker.close()
        if not args.no_preview:
            cv2.destroyAllWindows()
        emit({"type": "status", "status": "stopped", "frames": frames}, post_url)
    return 0


if __name__ == "__main__":
    sys.exit(main())
