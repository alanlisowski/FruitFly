#!/usr/bin/env python3
"""
Population firing rates under a few conditions. Use this to tune a pack:
a population at 0 Hz is under threshold, one pinned near 1/refractory
(~450 Hz) is saturated, and neither tells you anything useful.
"""
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from sim import Brain
from make_stub_pack import N_WEDGE

PACK = Path(__file__).parent.parent / "data" / "brain_stub.fbp"


def rates(b):
    out = defaultdict(list)
    for i, n in enumerate(b.meta["neurons"]):
        out[f"{n['type']}/{n['side']}"].append(b.rate[i])
    return {k: float(np.mean(v)) for k, v in sorted(out.items())}


def run(label, drive=None, ms=900, seed=7):
    b = Brain(PACK, seed=seed, noise_mv=0.12)
    epg = b.sensory["EPG"]
    for t in range(140):
        b.inject(epg[9:12], 16.0)
        b.step()
    for t in range(ms):
        if drive:
            drive(b, t, ms)
        b.step()

    print(f"\n=== {label} ===")
    ang, mag = b.bump_position()
    print(f"bump at {np.degrees(ang):+7.1f} deg, magnitude {mag:.3f}, "
          f"turn {b.turn_command():+7.1f}, forward {b.forward_command():6.1f}")

    # Bump profile across wedges.
    prof = np.array([b.rate[i] for i in epg]).reshape(N_WEDGE, -1).mean(axis=1)
    bars = "".join("#" if v > prof.max() * 0.5 else ("+" if v > prof.max() * 0.15 else ".")
                   for v in prof) if prof.max() > 0 else "." * len(prof)
    print(f"EPG profile [{bars}]  peak {prof.max():.0f} Hz, "
          f"wedges above half-peak: {(prof > prof.max() * 0.5).sum() if prof.max() > 0 else 0}")

    for k, v in rates(b).items():
        if v > 0.5:
            print(f"  {k:22s} {v:7.1f} Hz")
    return b


if __name__ == "__main__":
    run("rest")

    def pen_left(b, t, ms):
        b.inject([i for i, n in enumerate(b.meta["neurons"])
                  if n["type"] == "PEN_a" and n["side"] == "left"], 9.0)
    run("left PEN drive", pen_left)

    def loom_left(b, t, ms):
        b.inject(b.sensory["LPLC2"][:8], 16.0 * t / ms)
        b.inject(b.sensory["LC4"][:8], 11.0 * t / ms)
    run("loom on the left", loom_left)
