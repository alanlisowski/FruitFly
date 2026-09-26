#!/usr/bin/env python3
"""
Does the circuit do what the biology says it should?

Four checks. If these pass with the stub pack, the runtime is sound and the
only thing left is swapping hand-written weights for measured ones. If they
fail with the real pack, the extraction is wrong -- not the idea.

  1. A bump forms and stays put.       (ring attractor is stable)
  2. Driving one side rotates it.      (PEN offset integrates turning)
  3. Looming triggers escape.          (LPLC2 -> DNp01/DNp02)
  4. The escape is directional.        (loom on the left turns you right)
"""

import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from sim import Brain

PACK = Path(__file__).parent.parent / "data" / "brain_stub.fbp"


def settle(b, ms, drive=None):
    for _ in range(ms):
        if drive:
            drive(b)
        b.step()


def idx(b, type_, side=None):
    out = []
    for i, nrn in enumerate(b.meta["neurons"]):
        if nrn["type"] == type_ and (side is None or nrn["side"] == side):
            out.append(i)
    return out


def test_bump_forms_and_holds():
    b = Brain(PACK, seed=1, noise_mv=0.12)
    epg = b.sensory["EPG"]

    # Kick one wedge to seed a bump, then leave it alone.
    seed_wedge = epg[9:12]   # one wedge, 3 EPG each
    settle(b, 150, lambda br: br.inject(seed_wedge, 14.0))
    settle(b, 400)

    ang0, mag0 = b.bump_position()
    settle(b, 600)
    ang1, mag1 = b.bump_position()

    drift = abs(np.angle(np.exp(1j * (ang1 - ang0))))
    print(f"  bump magnitude after seeding: {mag0:.3f}")
    print(f"  bump magnitude 600 ms later:  {mag1:.3f}")
    print(f"  drift over 600 ms:            {np.degrees(drift):.1f} deg")

    assert mag1 > 0.25, f"no coherent bump (magnitude {mag1:.3f})"
    assert np.degrees(drift) < 60, f"bump drifted {np.degrees(drift):.0f} deg unprompted"
    return b


def test_bump_rotates():
    """
    Drive one side's PENs and the bump should walk that way.

    Judged over several seeds on purpose. With only 16 wedges the attractor is
    lumpy -- the bump prefers to sit on a wedge rather than between two -- so a
    single trial can stick or jump two wedges at once. What must hold is that
    the two sides push it in opposite directions on average.
    """
    shifts = {"left": [], "right": []}
    for seed in range(1, 7):
        for side in ("left", "right"):
            b = Brain(PACK, seed=seed, noise_mv=0.12)
            settle(b, 200, lambda br: br.inject(br.sensory["EPG"][9:12], 16.0))
            settle(b, 400)
            before, _ = b.bump_position()
            settle(b, 500, lambda br: br.inject(idx(br, "PEN_a", side), 9.0))
            after, mag = b.bump_position()
            shifts[side].append(np.degrees(np.angle(np.exp(1j * (after - before)))))

    mean_l = float(np.mean(shifts["left"]))
    mean_r = float(np.mean(shifts["right"]))
    agree = sum(1 for l, r in zip(shifts["left"], shifts["right"]) if np.sign(l) != np.sign(r))

    print(f"  left PEN drive  -> {mean_l:+.1f} deg / 500 ms (mean of 6)")
    print(f"  right PEN drive -> {mean_r:+.1f} deg / 500 ms (mean of 6)")
    print(f"  opposite directions in {agree}/6 seeds")

    assert mean_l > 8, f"left PEN drive barely moved the bump ({mean_l:+.1f} deg)"
    assert mean_r < -3, f"right PEN drive barely moved the bump ({mean_r:+.1f} deg)"
    assert agree >= 5, f"sides disagreed on direction in {6 - agree}/6 seeds"


def test_looming_triggers_escape():
    b = Brain(PACK, seed=3, noise_mv=0.12)
    settle(b, 200)
    baseline_run = b.role_rate("escape_run", "left") + b.role_rate("escape_run", "right")

    lplc2 = b.sensory["LPLC2"]
    lc4 = b.sensory["LC4"]
    # A loom ramps: slow at first, then hard as the object fills the eye.
    for t in range(300):
        drive = 16.0 * (t / 300.0) ** 2
        b.inject(lplc2, drive)
        b.inject(lc4, drive * 0.7)
        b.step()

    run = b.role_rate("escape_run", "left") + b.role_rate("escape_run", "right")
    jump = b.role_rate("escape_jump", "left") + b.role_rate("escape_jump", "right")

    print(f"  DNp02 (run)  baseline {baseline_run:6.1f} Hz -> loom {run:6.1f} Hz")
    print(f"  DNp01 (jump) at peak loom: {jump:.1f} Hz")

    assert run > baseline_run + 20, "looming did not drive the escape-run pathway"
    assert run > jump, "giant fibre fires more readily than DNp02 -- threshold is too low"


def test_escape_is_directional():
    results = {}
    for side, lo, hi in (("left", 0, 8), ("right", 8, 16)):
        b = Brain(PACK, seed=4, noise_mv=0.12)
        settle(b, 200)
        sector = b.sensory["LPLC2"][lo:hi]
        for t in range(250):
            b.inject(sector, 16.0 * (t / 250.0))
            b.step()
        results[side] = b.turn_command()

    print(f"  loom on the left  -> turn command {results['left']:+.1f} (positive = right)")
    print(f"  loom on the right -> turn command {results['right']:+.1f}")

    assert results["left"] > results["right"], \
        "fly does not turn away from the side the threat is on"


if __name__ == "__main__":
    if not PACK.exists():
        raise SystemExit(f"{PACK} not found -- run make_stub_pack.py first")

    tests = [
        ("bump forms and holds", test_bump_forms_and_holds),
        ("bump rotates with PEN drive", test_bump_rotates),
        ("looming triggers escape", test_looming_triggers_escape),
        ("escape is directional", test_escape_is_directional),
    ]

    failed = 0
    for name, fn in tests:
        print(f"\n{name}")
        try:
            fn()
            print("  PASS")
        except AssertionError as e:
            print(f"  FAIL: {e}")
            failed += 1

    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    sys.exit(1 if failed else 0)
