#!/usr/bin/env python3
"""
Build a stand-in brain.fbp so you can run the whole stack before downloading
250 MB of connectome.

This is NOT the fly's brain. It is a circuit with the same *topology* as the
blocks we extract from FlyWire -- a ring attractor with the EPG/PEN/Delta7
motif, a looming detector feeding the giant fibre, PFL3 reading the heading
bump out onto a left/right DN pair -- but with weights I wrote by hand from the
published circuit diagrams rather than measured synapse counts.

Its purpose is to prove the runtime, the pack format and the clients all work
end to end. Swap in the real pack from extract_circuit.py and every downstream
component is unchanged.

    python make_stub_pack.py --out data/brain_stub.fbp
"""

import argparse
from collections import defaultdict
from pathlib import Path
import sys

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from pack import write_pack
from celltypes import NT_SIGN

N_WEDGE = 16          # angular resolution of the ring attractor
N_LOOM = 16           # looming detectors, one per visual sector

# Tunable strengths, in synapses. Defaults below were found by sweeping for a
# bump 3-5 wedges wide firing 40-160 Hz -- the range recorded in real EPG cells.
DEFAULTS = {
    "epg_per_wedge": 3,
    "w_epg_self": 30, "w_epg_near": 14, "w_epg_far": 5,
    "w_epg_d7": 16, "w_d7_epg": 24,
    "w_epg_pen": 20, "w_pen_epg": 52,
    "w_er_epg": 26,
    "w_epg_pfl3": 18, "w_pfl3_dn": 14,
    "w_epg_pfl2": 14, "w_pfl2_dn": 30,
    "w_loom_turn": 14,
    "w_contact_dn": 20,
    "w_contact_near_dn": 20,
}

# Same conversion the real extractor uses, so stub weights and measured weights
# are on one scale and you can compare them directly.
MV_PER_SYNAPSE = 0.275

# Connection strengths, quoted in synapses because that is what the connectome
# actually measures. FlyWire's weak-edge cutoff is 5, a typical strong pathway
# is 30-60, and the heaviest pairs in the central complex run into the hundreds.
# Tonic background depolarisation in mV, applied every step to each neuron of
# the named type. The ellipsoid body receives steady input in the real animal;
# without it a spiking ring attractor this size decays inside a second. Swept
# value: 4.5 mV holds a 5-wedge bump near 160 Hz drifting about 1 deg/s.
TONIC = {"EPG": 4.5}

STRONG = 30
MEDIUM = 18
WEAK = 8


def build(P=None):
    P = {**DEFAULTS, **(P or {})}
    neurons = []          # (type, side, nt, block)
    edges = defaultdict(float)   # (pre, post) -> mV

    def add(type_, side, nt, block):
        neurons.append({"id": len(neurons), "type": type_, "side": side,
                        "nt": nt, "block": block})
        return len(neurons) - 1

    def connect(pre, post, synapses):
        """Sign comes from the presynaptic transmitter, exactly as in the
        real extractor -- so an inhibitory cell cannot accidentally be wired
        up as excitatory here."""
        sign = NT_SIGN.get(neurons[pre]["nt"], 1.0)
        edges[(pre, post)] += synapses * MV_PER_SYNAPSE * sign

    # ---- ring attractor -------------------------------------------------
    # EPG: the heading bump. FlyWire has ~46 EPG neurons, so ~3 per wedge.
    # This number matters more than it looks: with only one or two cells per
    # wedge the bump cannot sustain itself, because a single presynaptic spike
    # delivers far less depolarisation than its weight suggests. The synapse
    # decays with tau 5 ms while the membrane integrates with tau 20 ms, so a
    # lone 9 mV spike moves the membrane under 2 mV. Recurrent excitation only
    # works when enough cells fire together.
    epg = [[add("EPG", "left" if k < N_WEDGE // 2 else "right", "ACH", "central_complex")
            for _ in range(P["epg_per_wedge"])] for k in range(N_WEDGE)]
    # PEN: rotate the bump. Left PEN shifts it one way, right PEN the other.
    pen_l = [add("PEN_a", "left", "ACH", "central_complex") for _ in range(N_WEDGE)]
    pen_r = [add("PEN_a", "right", "ACH", "central_complex") for _ in range(N_WEDGE)]
    # Delta7: global inhibition. Keeps exactly one bump alive.
    d7 = [add("Delta7", "centre", "GLUT", "central_complex") for _ in range(8)]
    # ER: visual ring neurons, inhibitory, pin the bump to what the fly sees.
    er = [add("ER2_a", "centre", "GABA", "central_complex") for _ in range(N_WEDGE)]

    # Local recurrent excitation: each wedge excites itself and its neighbours.
    for k in range(N_WEDGE):
        for a in epg[k]:
            for dk, g in ((0, P["w_epg_self"]), (1, P["w_epg_near"]), (-1, P["w_epg_near"]),
                          (2, P["w_epg_far"]), (-2, P["w_epg_far"])):
                for b in epg[(k + dk) % N_WEDGE]:
                    if a != b:
                        connect(a, b, g)

    # EPG -> PEN, same wedge. PEN -> EPG, shifted by one wedge. That offset is
    # the whole trick: drive the PENs on one side and the bump walks around.
    for k in range(N_WEDGE):
        for a in epg[k]:
            connect(a, pen_l[k], P["w_epg_pen"])
            connect(a, pen_r[k], P["w_epg_pen"])
        for b in epg[(k + 1) % N_WEDGE]:
            connect(pen_l[k], b, P["w_pen_epg"])
        for b in epg[(k - 1) % N_WEDGE]:
            connect(pen_r[k], b, P["w_pen_epg"])

    # EPG -> Delta7 -> everything. Global winner-take-all.
    for k in range(N_WEDGE):
        for a in epg[k]:
            connect(a, d7[k % len(d7)], P["w_epg_d7"])
    for d in d7:
        for k in range(N_WEDGE):
            for b in epg[k]:
                connect(d, b, P["w_d7_epg"])

    # ER -> EPG, inhibitory and retinotopic.
    for k in range(N_WEDGE):
        for b in epg[k]:
            connect(er[k], b, P["w_er_epg"])

    # ---- steering readout ----------------------------------------------
    # PFL3 reads the bump with a ~90 degree offset, left and right copies
    # offset in opposite directions. Where the bump sits relative to the goal
    # decides which side wins.
    pfl3_l = [add("PFL3", "left", "ACH", "central_complex") for _ in range(N_WEDGE)]
    pfl3_r = [add("PFL3", "right", "ACH", "central_complex") for _ in range(N_WEDGE)]
    for k in range(N_WEDGE):
        for a in epg[k]:
            connect(a, pfl3_l[(k + N_WEDGE // 4) % N_WEDGE], P["w_epg_pfl3"])
            connect(a, pfl3_r[(k - N_WEDGE // 4) % N_WEDGE], P["w_epg_pfl3"])

    # DNa02: the see-saw. Each side is driven by its own PFL3 population and
    # inhibits the other side. Turn rate is right-minus-left firing rate.
    dna02_l = [add("DNa02", "left", "ACH", "descending") for _ in range(3)]
    dna02_r = [add("DNa02", "right", "ACH", "descending") for _ in range(3)]
    dna01_l = [add("DNa01", "left", "ACH", "descending") for _ in range(2)]
    dna01_r = [add("DNa01", "right", "ACH", "descending") for _ in range(2)]
    for k in range(N_WEDGE):
        for d in dna02_l:
            connect(pfl3_l[k], d, P["w_pfl3_dn"])
        for d in dna02_r:
            connect(pfl3_r[k], d, P["w_pfl3_dn"])
        for d in dna01_l:
            connect(pfl3_l[k], d, 12)
        for d in dna01_r:
            connect(pfl3_r[k], d, 12)

    # ---- forward drive ---------------------------------------------------
    # PFL2 sets how hard the fly drives forward. It pools the heading bump
    # broadly rather than sampling one wedge, so forward drive reflects how
    # strong the bump is overall, not where it happens to sit.
    pfl2 = [add("PFL2", "centre", "ACH", "central_complex") for _ in range(8)]
    for k in range(N_WEDGE):
        for a in epg[k]:
            connect(a, pfl2[k % len(pfl2)], P["w_epg_pfl2"])
            connect(a, pfl2[(k + 1) % len(pfl2)], P["w_epg_pfl2"])
    bdn2 = [add("BDN2", "centre", "ACH", "descending") for _ in range(2)]
    dnb02 = [add("DNb02", s, "ACH", "descending") for s in ("left", "right")]
    for p in pfl2:
        for d in bdn2 + dnb02:
            connect(p, d, P["w_pfl2_dn"])

    # ---- looming and escape ---------------------------------------------
    # LPLC2 tiles the visual field. Radial expansion in its sector drives it.
    lplc2 = [add("LPLC2", "left" if k < N_LOOM // 2 else "right", "ACH", "looming")
             for k in range(N_LOOM)]
    lc4 = [add("LC4", "left" if k < N_LOOM // 2 else "right", "ACH", "looming")
           for k in range(N_LOOM)]
    # Giant fibre. Needs a lot of coincident LPLC2 input to fire -- it should
    # be hard to trigger, or the fly jumps at everything.
    dnp01 = [add("DNp01", s, "ACH", "descending") for s in ("left", "right")]
    dnp02 = [add("DNp02", s, "ACH", "descending") for s in ("left", "right")]
    dnp09 = [add("DNp09", s, "ACH", "descending") for s in ("left", "right")]
    for k in range(N_LOOM):
        for d in dnp01:
            connect(lplc2[k], d, 3)
            connect(lc4[k], d, 2)
        for d in dnp02:
            connect(lplc2[k], d, 9)
            connect(lc4[k], d, 7)
        for d in dnp09:
            connect(lplc2[k], d, 3)

    # Escape suppresses steering while it is running. DNp02 is cholinergic, so
    # it cannot inhibit anything directly -- the suppression goes through a
    # GABAergic gate interneuron. Getting this right matters: wiring an
    # excitatory cell up as inhibitory is the classic way to make a connectome
    # model behave plausibly for the wrong reason.
    gate = [add("LAL010", s, "GABA", "descending") for s in ("left", "right")]
    for d in dnp02:
        for gg in gate:
            connect(d, gg, 24)
    for gg in gate:
        for t in dna02_l + dna02_r:
            connect(gg, t, 20)

    # Looming on one side pushes the turn away from that side.
    for k in range(N_LOOM):
        targets = dna02_r if k < N_LOOM // 2 else dna02_l
        for d in targets:
            connect(lplc2[k], d, P["w_loom_turn"])

    # ---- optic flow -> heading update ------------------------------------
    # HS cells report horizontal motion; they drive the PENs so that visual
    # rotation updates the heading estimate.
    hs_l = [add("HSE", "left", "ACH", "optic_lobe") for _ in range(2)]
    hs_r = [add("HSE", "right", "ACH", "optic_lobe") for _ in range(2)]
    for h in hs_l:
        for p in pen_l:
            connect(h, p, 16)
    for h in hs_r:
        for p in pen_r:
            connect(h, p, 16)

    # ---- grooming --------------------------------------------------------
    dng11 = [add("DNg11", s, "ACH", "descending") for s in ("left", "right")]

    # ---- touch -----------------------------------------------------------
    # CONTACT stands in for mechanosensory input (antennae, legs) reporting an edge within
    # reach on one side. Each side excites its own DNa02, so the fly turns toward what it
    # touches; crossing the edge moves it to the other side and turns the fly back. That weave
    # is the edge following. Appended last, so no existing neuron index changes.
    contact_l = [add("CONTACT", "left", "ACH", "mechanosensory") for _ in range(4)]
    contact_r = [add("CONTACT", "right", "ACH", "mechanosensory") for _ in range(4)]
    for c in contact_l:
        for d in dna02_l:
            connect(c, d, P["w_contact_dn"])
    for c in contact_r:
        for d in dna02_r:
            connect(c, d, P["w_contact_dn"])
    # CONTACT_NEAR: the same touch at close range (~0.3 body lengths), wired CROSSED, so the
    # fly turns away from an edge it's almost on. Far pulls in, near pushes out: together they
    # hold the fly a little way off the edge, alongside it (a Braitenberg wall follower).
    near_l = [add("CONTACT_NEAR", "left", "ACH", "mechanosensory") for _ in range(4)]
    near_r = [add("CONTACT_NEAR", "right", "ACH", "mechanosensory") for _ in range(4)]
    for c in near_l:
        for d in dna02_r:
            connect(c, d, P["w_contact_near_dn"])
    for c in near_r:
        for d in dna02_l:
            connect(c, d, P["w_contact_near_dn"])

    motor = defaultdict(lambda: {"left": [], "right": [], "other": []})

    def tag(role, left, right, other=()):
        motor[role]["left"].extend(left)
        motor[role]["right"].extend(right)
        motor[role]["other"].extend(other)

    tag("turn_fast", dna02_l, dna02_r)
    tag("turn_sustained", dna01_l, dna01_r)
    tag("forward_fast", [], [], bdn2)
    tag("forward", [dnb02[0]], [dnb02[1]])
    tag("escape_jump", [dnp01[0]], [dnp01[1]])
    tag("escape_run", [dnp02[0]], [dnp02[1]])
    tag("stop", [dnp09[0]], [dnp09[1]])
    tag("groom", [dng11[0]], [dng11[1]])

    sensory = {
        "LPLC2": lplc2,
        "LC4": lc4,
        "ring_visual": er,
        "HSE_left": hs_l,
        "HSE_right": hs_r,
        "CONTACT_left": contact_l,
        "CONTACT_right": contact_r,
        "CONTACT_NEAR_left": near_l,
        "CONTACT_NEAR_right": near_r,
        "EPG": [i for w in epg for i in w],
    }

    return neurons, edges, dict(motor), sensory


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=Path("data/brain_stub.fbp"))
    args = ap.parse_args()
    args.out.parent.mkdir(parents=True, exist_ok=True)

    neurons, edges, motor, sensory = build()
    n = len(neurons)

    pre = np.array([e[0] for e in edges], dtype=np.int64)
    post = np.array([e[1] for e in edges], dtype=np.int64)
    w = np.array(list(edges.values()), dtype=np.float32)

    order = np.argsort(pre, kind="stable")
    pre, post, w = pre[order], post[order], w[order]
    indptr = np.zeros(n + 1, dtype=np.uint32)
    np.cumsum(np.bincount(pre, minlength=n), out=indptr[1:])

    meta = {
        "version": "stub-not-real-connectome",
        "source": "hand-built from published circuit diagrams; NOT measured synapse counts",
        "neurons": neurons,
        "motor": motor,
        "sensory": sensory,
        # Tonic background depolarisation, in mV, applied every step to every
        # neuron of the named type. The ellipsoid body receives steady input in
        # the real animal; without it a spiking ring attractor of this size
        # decays within a second. Swept value: 4.5 mV holds a 5-wedge bump at
        # ~160 Hz drifting 1.2 deg/s, against ~1 deg/s measured in darkness.
        "tonic": dict(TONIC),
        "params": {
            "tau_membrane_ms": 20.0,
            "tau_synapse_ms": 5.0,
            "v_rest_mv": -52.0,
            "v_threshold_mv": -45.0,
            "v_reset_mv": -52.0,
            "refractory_ms": 2.2,
            "dt_ms": 1.0,
        },
    }

    stats = write_pack(args.out, indptr, post, w, meta)
    print(f"wrote {args.out}: {stats['neurons']} neurons, {stats['edges']} edges, "
          f"{stats['bytes'] / 1e3:.1f} kB")


if __name__ == "__main__":
    main()
