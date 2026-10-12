"""Linux action executors for gesture events. No new system dependencies.

Media keys go straight to MPRIS players over D-Bus (gdbus/dbus-send are
preinstalled; playerctl is NOT required). Volume goes through pactl.
Everything runs in dry-run-safe mode: with dry_run=True nothing executes.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from dataclasses import dataclass


def _run(argv: list[str], dry_run: bool) -> tuple[bool, str]:
    cmd = " ".join(argv)
    if dry_run:
        return True, f"[dry-run] {cmd}"
    try:
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=10)
    except FileNotFoundError:
        return False, f"missing binary: {argv[0]}"
    except subprocess.TimeoutExpired:
        return False, f"timed out: {cmd}"
    if proc.returncode != 0:
        return False, f"{cmd} -> {proc.stderr.strip() or proc.returncode}"
    return True, proc.stdout.strip() or "ok"


def _mpris_players() -> list[str]:
    """Well-known bus names of MPRIS-capable media players, if any."""
    if not shutil.which("dbus-send"):
        return []
    try:
        proc = subprocess.run(
            ["dbus-send", "--session", "--print-reply=literal",
             "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus",
             "org.freedesktop.DBus.ListNames"],
            capture_output=True, text=True, timeout=5,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return []
    return re.findall(r'"([^"]*org\.mpris\.MediaPlayer2[^"]*)"', proc.stdout)


def _mpris(method: str, dry_run: bool) -> tuple[bool, str]:
    players = _mpris_players()
    if not players:
        return (True, "[dry-run] MPRIS " + method) if dry_run else (False, "no MPRIS players found")
    results = []
    for player in players:
        ok, msg = _run(
            ["dbus-send", "--session", "--type=method_call",
             f"--dest={player}", "/org/mpris/MediaPlayer2",
             f"org.mpris.MediaPlayer2.Player.{method}"],
            dry_run,
        )
        results.append(f"{player.split('.')[-1]}: {'ok' if ok else msg}")
    return True, "; ".join(results)


@dataclass
class ActionRunner:
    dry_run: bool = True

    def run(self, action: str) -> tuple[bool, str]:
        if action == "play_pause":
            return _mpris("PlayPause", self.dry_run)
        if action == "next_track":
            return _mpris("Next", self.dry_run)
        if action == "prev_track":
            return _mpris("Previous", self.dry_run)
        if action == "toggle_mute":
            return _run(["pactl", "set-sink-mute", "@DEFAULT_SINK@", "toggle"], self.dry_run)
        if action == "volume_up":
            return _run(["pactl", "set-sink-volume", "@DEFAULT_SINK@", "+5%"], self.dry_run)
        if action == "volume_down":
            return _run(["pactl", "set-sink-volume", "@DEFAULT_SINK@", "-5%"], self.dry_run)
        return False, f"unknown action: {action}"
