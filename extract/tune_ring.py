#!/usr/bin/env python3
"""
Sweep the ring attractor's two opposing forces -- local recurrent excitation
and Delta7 global inhibition -- and find the window where a bump both survives
on its own and stays narrow.

Too little excitation: the bump dies the moment you stop driving it.
Too much, or too little inhibition: every wedge fires and there is no bump.
The useful region is narrow, which is exactly why it is worth measuring
instead of guessing.
"""
import sys
import tempfile
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
import make_stub_pack as msp
from pack import write_pack
from sim import Brain


def build_to(path, P):
    neurons, edges, motor, sensory = msp.build(P)
    n = len(neurons)
    pre = np.array([e[0] for e in edges], dtype=np.int64)
    post = np.array([e[1] for e in edges], dtype=np.int64)
    w = np.array(list(edges.values()), dtype=np.float32)
    o = np.argsort(pre, kind="stable")
    pre, post, w = pre[o], post[o], w[o]
    indptr = np.zeros(n + 1, dtype=np.uint32)
    np.cumsum(np.bincount(pre, minlength=n), out=indptr[1:])
    # Build the meta the same way make_stub_pack does, tonic drive included --
    # a sweep that quietly drops it measures a different circuit.
    meta = {"version": "sweep", "neurons": neurons, "motor": motor, "sensory": sensory,
            "tonic": dict(msp.TONIC),
            "params": {"tau_membrane_ms": 20.0, "tau_synapse_ms": 5.0, "v_rest_mv": -52.0,
                       "v_threshold_mv": -45.0, "v_reset_mv": -52.0,
                       "refractory_ms": 2.2, "dt_ms": 1.0}}
    write_pack(path, indptr, post, w, meta)


def evaluate(P, seed=3):
    """Seed a bump, stop driving, and see what is left 800 ms later."""
    with tempfile.NamedTemporaryFile(suffix=".fbp", delete=False) as fh:
        path = fh.name
    build_to(path, P)
    b = Brain(path, seed=seed, noise_mv=0.15)
    Path(path).unlink()

    per = P.get("epg_per_wedge", msp.DEFAULTS["epg_per_wedge"])
    epg = b.sensory["EPG"]
    wedge = epg[3 * per:4 * per]           # seed wedge 3

    for _ in range(150):
        b.inject(wedge, 16.0)
        b.step()
    for _ in range(800):
        b.step()

    prof = np.array([b.rate[i] for i in epg]).reshape(-1, per).mean(axis=1)
    peak = float(prof.max())
    if peak < 1.0:
        return {"peak": peak, "width": 0, "ok": False, "why": "bump died"}
    width = int((prof > peak * 0.5).sum())
    total = float(prof.mean())
    ok = 25 <= peak <= 220 and 2 <= width <= 6
    why = ("peak too low" if peak < 25 else "peak saturated" if peak > 220
           else "bump too narrow" if width < 2 else "no bump, all wedges on" if width > 6
           else "ok")
    return {"peak": peak, "width": width, "mean": total, "ok": ok, "why": why}


if __name__ == "__main__":
    print(f"{'self':>5} {'near':>5} {'d7':>4} | {'peak Hz':>8} {'width':>6}  verdict")
    print("-" * 52)
    best = []
    for self_w in (14, 18, 22, 26, 30):
        for near in (10, 14, 18):
            for d7 in (6, 9, 12, 16):
                P = {"w_epg_self": self_w, "w_epg_near": near, "w_d7_epg": d7}
                r = evaluate(P)
                flag = "  <== " if r["ok"] else ""
                print(f"{self_w:5d} {near:5d} {d7:4d} | {r['peak']:8.1f} {r['width']:6d}"
                      f"  {r['why']}{flag}")
                if r["ok"]:
                    best.append((abs(r["peak"] - 90) + abs(r["width"] - 4) * 20, P, r))

    if best:
        best.sort(key=lambda x: x[0])
        score, P, r = best[0]
        print(f"\nbest: {P}  ->  peak {r['peak']:.0f} Hz, width {r['width']} wedges")
    else:
        print("\nnothing in range -- widen the sweep")
