#!/usr/bin/env python3
"""Compare adaptive-bitrate simulation runs (testing.md §3.3), e.g. before and after a change.

usage: scripts/sim-compare.py <before.txt> <after.txt> [more.txt...]

Each file is the output of
    cargo test --release -p mirrorwire-proto sim_ -- --nocapture --test-threads=1 > F 2>&1
(add MIRRORWIRE_SIM_SOFT=1 so a failing limit does not stop the run before every row is
printed, and MIRRORWIRE_SIM_WIDE=1 to average over 20 ceilings instead of 5 when tuning).

Prints one totals line per file (frozen frames summed over the averaged rows, mean video,
cuts summed, soft failures), then every row that differs between the first two files.
Burst rows (the unpaced baseline of the keyframe test) are left out.
"""
import os
import re
import sys
import tempfile
from pathlib import Path

MEAN = re.compile(
    r"(.*?): (paced|fresh|resumed) Mean \{ cuts: ([\d.]+), lost_frames: ([\d.]+), "
    r"video_kbps: ([\d.]+), lost_pct: ([\d.]+), frozen_frames: ([\d.]+), lag_p95_ms: ([\d.]+)"
)
SINGLE = re.compile(
    r"(.*?): (\d+) cuts, final (\d+).*delivered video (\d+) kbit/s, lost ([\d.]+) %, "
    r"queue mean ([\d.]+) p95 ([\d.]+)"
)


def safe_input(arg):
    """Resolve a command-line path and accept it only inside an allowed base directory.

    Allowed: the current directory, the repository, the system temp directory (the run scripts
    log to /tmp) and any directory listed in MIRRORWIRE_SCRIPT_BASE (os.pathsep separated).
    Anything else, or a path that is not a regular file, ends the script with an error.
    """
    bases = [Path.cwd(), Path(__file__).resolve().parent.parent, Path(tempfile.gettempdir())]
    bases += [Path(b) for b in os.environ.get("MIRRORWIRE_SCRIPT_BASE", "").split(os.pathsep) if b]
    resolved = Path(arg).resolve()
    if not any(resolved.is_relative_to(b.resolve()) for b in bases):
        sys.exit(f"{arg}: outside the allowed directories (cwd, repo, temp, MIRRORWIRE_SCRIPT_BASE)")
    if not resolved.is_file():
        sys.exit(f"{arg}: not a regular file")
    return resolved


def parse(path):
    rows, totals = {}, {"frozen": 0.0, "video": 0.0, "cuts": 0.0, "n": 0}
    text = safe_input(path).read_text(errors="replace")
    for line in text.split("\n"):
        if m := MEAN.match(line):
            cuts, lost_fr, video, lost, frozen, lag = map(float, m.groups()[2:])
            rows[f"{m[1]} {m[2]}"] = (
                f"frozen {frozen:7.1f} lost fr {lost_fr:6.1f} video {video / 1000:5.1f} "
                f"loss {lost:5.2f} % cuts {cuts:4.1f} lag p95 {lag:4.0f}"
            )
            totals["frozen"] += frozen
            totals["video"] += video
            totals["cuts"] += cuts
            totals["n"] += 1
        elif m := SINGLE.match(line):
            name = m[1].removeprefix("test ").split(" ... ")[-1]
            rows[name] = (
                f"cuts {m[2]:>3} video {int(m[4]) / 1000:5.1f} loss {float(m[5]):5.2f} % "
                f"queue p95 {float(m[7]):5.1f}"
            )
    totals["fails"] = text.count("SOFT FAIL") + text.count("panicked")
    return rows, totals


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    parsed = [(p, *parse(p)) for p in sys.argv[1:]]
    for path, _, t in parsed:
        video = t["video"] / max(t["n"], 1) / 1000
        print(
            f"{path}: frozen sum {t['frozen']:.0f}, video mean {video:.2f} Mbit/s, "
            f"cuts sum {t['cuts']:.1f}, failures {t['fails']}"
        )
    (_, a, _), (_, b, _) = parsed[0], parsed[1]
    print()
    for key in a:
        if a[key] != b.get(key):
            print(f"{key[:50]:<50} {a[key]}\n{'':<50} {b.get(key, 'missing')}")
    for key in b.keys() - a.keys():
        print(f"{key[:50]:<50} new: {b[key]}")


if __name__ == "__main__":
    main()
