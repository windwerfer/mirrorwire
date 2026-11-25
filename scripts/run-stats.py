#!/usr/bin/env python3
"""Summarise presenter logs, one row per run (scripts/android-wifi-run.sh, or any presenter log).

usage: scripts/run-stats.py <presenter-log>...

From the per-second `presenter:` lines that carry a viewer report: mean video sent, keyframes,
queue mean / p95 (of the per-second viewer queue), QUIC packets lost, and the viewer's
unrecoverable frames (a running total); from `adaptive bitrate: cut` lines: cuts per trigger;
from `adaptive bitrate: probe` lines: parity probes started / clean / ended by a cut.
If `<presenter-log>.logcat` exists (the Android viewer's log), also: mean fps shown, frames
skipped while waiting for a keyframe (the picture is frozen then), and seconds with a
"Picture frozen" warning.
"""
import os
import re
import statistics as st
import sys
import tempfile
from pathlib import Path

STATS = re.compile(r" presenter: [\d.]+ fps")


def safe_input(arg):
    """Resolve a command-line path and accept it only inside an allowed base directory.

    Allowed: the current directory, the repository, the system temp directory (the run scripts
    log to /tmp) and any directory listed in MIRRORWIRE_SCRIPT_BASE (os.pathsep separated).
    Anything else, or a path that is not a regular file, ends the script with an error.
    """
    bases = [Path.cwd(), Path(__file__).resolve().parent.parent, Path(tempfile.gettempdir())]
    bases += [Path(b) for b in os.environ.get("MIRRORWIRE_SCRIPT_BASE", "").split(os.pathsep) if b]
    resolved = Path(os.path.realpath(arg))
    if not any(resolved.is_relative_to(os.path.realpath(b)) for b in bases):
        sys.exit(f"{arg}: outside the allowed directories (cwd, repo, temp, MIRRORWIRE_SCRIPT_BASE)")
    if not resolved.is_file():
        sys.exit(f"{arg}: not a regular file")
    return resolved


def summarise(path):
    lines = safe_input(path).read_text(errors="replace").split("\n")
    rows = [l for l in lines if STATS.search(l) and "viewer queue" in l]
    num = lambda pat, l: re.search(pat, l)
    video = [float(num(r"fps, ([\d.]+) Mbit/s", l).group(1)) for l in rows]
    keyframes = sum(int(num(r"Mbit/s, (\d+) keyframes", l).group(1)) for l in rows)
    paced = sum(int(m.group(1)) for l in rows if (m := num(r"keyframes, (\d+) paced", l)))
    queue = sorted(float(num(r"viewer queue (\d+) ms", l).group(1)) for l in rows)
    quic = [tuple(map(int, m.groups())) for l in rows if (m := num(r"quic lost (\d+) of (\d+)", l))]
    unrec = [int(m.group(1)) for l in rows if (m := num(r"unrecoverable (\d+)", l))]
    triggers = {}
    for l in lines:
        if "adaptive bitrate: cut" not in l:
            continue
        reason = l.split("Mbit/s: ", 1)[-1]
        kind = next((k for k in ("heavy", "loss", "queue") if reason.startswith(k)), "backlog" if "send queue" in reason else "other")
        triggers[kind] = triggers.get(kind, 0) + 1
    lost, sent = sum(a for a, _ in quic), sum(b for _, b in quic)
    p95 = queue[max(int(len(queue) * 0.95) - 1, 0)] if queue else 0
    cuts = sum(triggers.values())
    keyframes = f"{keyframes} ({paced} frames paced)" if paced else keyframes
    probes = probe_part(lines)
    return summary_line(path, rows, video, keyframes, cuts, triggers, queue, p95, lost, sent, unrec) + probes + viewer(path + ".logcat")


def probe_part(lines):
    started = sum("adaptive bitrate: probe +" in l for l in lines)
    if not started:
        return ""
    clean = sum("adaptive bitrate: probe clean" in l for l in lines)
    failed = sum("adaptive bitrate: probe failed" in l for l in lines)
    return f", probes {started} ({clean} clean, {failed} cut)"


def viewer(logcat):
    try:
        text = safe_input(logcat).read_text(errors="replace")
    except SystemExit:
        return ""  # no .logcat next to the log (or not a regular file)
    lines = [l for l in text.split("\n") if " shown " in l and " fps" in l]
    shown = [float(m.group(1)) for l in lines if (m := re.search(r"shown ([\d.]+) fps", l))]
    waiting = sum(int(m.group(1)) for l in lines if (m := re.search(r"waiting key (\d+)/s", l)))
    frozen = sum("Picture frozen" in l for l in lines)
    return (
        f", viewer shown {st.mean(shown) if shown else 0:.1f} fps, "
        f"waiting for a keyframe {waiting} frames, frozen warnings {frozen} s"
    )


def summary_line(path, rows, video, keyframes, cuts, triggers, queue, p95, lost, sent, unrec):
    return (
        f"{path}: {len(rows)} s, video {st.mean(video) if video else 0:.1f} Mbit/s, "
        f"keyframes {keyframes}, cuts {cuts} {triggers or ''}, "
        f"queue mean {st.mean(queue) if queue else 0:.0f} p95 {p95:.0f} ms, "
        f"quic lost {lost}/{sent} ({100 * lost / max(sent, 1):.1f} %), "
        f"unrecoverable {unrec[-1] if unrec else '?'}"
    )


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    for p in sys.argv[1:]:
        print(summarise(p))
