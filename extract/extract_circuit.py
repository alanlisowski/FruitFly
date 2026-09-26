#!/usr/bin/env python3
"""
Turn the FlyWire connectome into a circuit small enough to run at 1 kHz.

Input (you download these once, see README):
    annotations.tsv     Supplemental_file1_neuron_annotations.tsv  (~25 MB)
    connections.csv.gz  Codex `connections_princeton` export       (~250 MB)
                        or proofread_connections_783.feather from Zenodo

Output:
    brain.fbp           a few MB, loads in ~1 ms

Usage:
    python extract_circuit.py \
        --annotations data/annotations.tsv \
        --connections data/connections.csv.gz \
        --out data/brain.fbp

What it actually does:

  1. Read the annotation table, keep only neurons whose cell_type is in our
     selection list (celltypes.py).
  2. Read the connection table, keep only edges where BOTH endpoints survived
     step 1.
  3. Drop weak edges. FlyWire's own convention is that a connection needs >= 5
     synapses to be considered reliable; below that you are looking at
     segmentation noise as much as biology.
  4. Convert synapse counts to signed millivolts using the presynaptic
     neuron's neurotransmitter.
  5. Tag the motor outputs and sensory inputs so the runtime knows which
     indices to read and write.
  6. Write the CSR pack.

The whole thing is a few minutes on a laptop, and you run it once.
"""

import argparse
import gzip
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
import pandas as pd

sys.path.insert(0, str(Path(__file__).parent))
import celltypes as ct
from pack import write_pack

# A connection needs at least this many synapses to be believed.
MIN_SYNAPSES = 5

# Millivolts of postsynaptic depolarisation per presynaptic spike, per synapse.
# From Shiu et al.'s whole-brain LIF model of this same connectome: 0.275 mV.
# With a 7 mV gap between rest (-52) and threshold (-45), that means roughly
# 25 coincident synapses to fire a silent neuron, which is the right ballpark.
MV_PER_SYNAPSE = 0.275


def load_annotations(path):
    df = pd.read_csv(path, sep="\t", low_memory=False)

    # The release has moved columns around between versions. Be forgiving.
    if "root_id" not in df.columns:
        raise SystemExit(f"{path} has no root_id column; got {list(df.columns)[:10]}")

    # Prefer the curated cell_type, fall back to the hemibrain match.
    if "cell_type" in df.columns and "hemibrain_type" in df.columns:
        df["type"] = df["cell_type"].fillna(df["hemibrain_type"])
    else:
        df["type"] = df.get("cell_type", df.get("hemibrain_type"))

    df["type"] = df["type"].astype("string").str.strip()
    df["side"] = df.get("side", pd.Series(["unknown"] * len(df))).astype("string")
    df["nt_type"] = df.get("nt_type", pd.Series(["UNK"] * len(df))).astype("string").str.upper().fillna("UNK")

    return df[["root_id", "type", "side", "nt_type"]].dropna(subset=["type"])


def load_connections(path):
    """
    Codex exports columns: pre_root_id, post_root_id, neuropil, syn_count, nt_type
    Zenodo feather uses:   pre_pt_root_id, post_pt_root_id, syn_count, ...
    Accept either.
    """
    p = str(path)
    if p.endswith(".feather"):
        df = pd.read_feather(p)
    elif p.endswith(".gz"):
        with gzip.open(p, "rt") as fh:
            df = pd.read_csv(fh)
    else:
        df = pd.read_csv(p)

    rename = {
        "pre_pt_root_id": "pre_root_id",
        "post_pt_root_id": "post_root_id",
        "syn_cnt": "syn_count",
        "count": "syn_count",
    }
    df = df.rename(columns={k: v for k, v in rename.items() if k in df.columns})

    need = {"pre_root_id", "post_root_id", "syn_count"}
    missing = need - set(df.columns)
    if missing:
        raise SystemExit(f"{path} is missing columns {missing}; got {list(df.columns)}")

    # The neuropil-split tables have one row per (pre, post, neuropil). Collapse.
    if "neuropil" in df.columns:
        df = df.groupby(["pre_root_id", "post_root_id"], as_index=False)["syn_count"].sum()

    return df[["pre_root_id", "post_root_id", "syn_count"]]


def block_of(cell_type):
    if cell_type in ct.OPTIC_LOBE:
        return "optic_lobe"
    if cell_type in ct.LOOMING:
        return "looming"
    if cell_type in ct.CENTRAL_COMPLEX:
        return "central_complex"
    if cell_type in ct.DESCENDING:
        return "descending"
    return "other"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--annotations", required=True, type=Path)
    ap.add_argument("--connections", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--min-synapses", type=int, default=MIN_SYNAPSES)
    ap.add_argument("--keep-optic-lobe", action="store_true",
                    help="Keep every optic lobe neuron individually (~70k, too slow "
                         "for real time). Default is to drop them here and tile a "
                         "canonical column at runtime instead.")
    args = ap.parse_args()

    print("reading annotations ...", flush=True)
    ann = load_annotations(args.annotations)
    print(f"  {len(ann):,} annotated neurons")

    wanted = ct.all_types()
    if not args.keep_optic_lobe:
        # Photoreceptors through T4/T5 get tiled at runtime from the canonical
        # column, so we do not need every instance. We DO keep the wide-field
        # tangential cells, which are individually identified and few.
        tiled = set(ct.PHOTORECEPTORS) | set(ct.LAMINA) | set(ct.MEDULLA) | set(ct.T4T5)
        wanted = wanted - tiled

    sel = ann[ann["type"].isin(wanted)].copy()
    print(f"  {len(sel):,} neurons match the selection")

    found = set(sel["type"].unique())
    absent = sorted(wanted - found)
    if absent:
        print(f"  note: {len(absent)} requested types absent from this release:")
        print("    " + ", ".join(absent[:20]) + (" ..." if len(absent) > 20 else ""))

    by_block = Counter(block_of(t) for t in sel["type"])
    for b, n in sorted(by_block.items(), key=lambda kv: -kv[1]):
        print(f"    {b:18s} {n:6,}")

    # Stable ordering: block, then type, then side, then root_id. Keeping this
    # deterministic means a re-extraction produces byte-identical packs, which
    # makes the runtime's behaviour reproducible.
    sel["block"] = sel["type"].map(block_of)
    sel = sel.sort_values(["block", "type", "side", "root_id"]).reset_index(drop=True)
    index_of = {int(r): i for i, r in enumerate(sel["root_id"])}
    n = len(sel)

    print("reading connections ...", flush=True)
    conn = load_connections(args.connections)
    print(f"  {len(conn):,} edges in the full connectome")

    conn = conn[conn["syn_count"] >= args.min_synapses]
    keep = set(index_of)
    conn = conn[conn["pre_root_id"].isin(keep) & conn["post_root_id"].isin(keep)]
    print(f"  {len(conn):,} edges inside the circuit (>= {args.min_synapses} synapses)")

    if len(conn) == 0:
        raise SystemExit("no edges survived -- check that the two files are the same release")

    # Sign each edge by its presynaptic neuron's neurotransmitter.
    nt_of = dict(zip(sel["root_id"].astype("int64"), sel["nt_type"]))
    pre_idx = conn["pre_root_id"].map(index_of).to_numpy(dtype=np.int64)
    post_idx = conn["post_root_id"].map(index_of).to_numpy(dtype=np.int64)
    signs = np.array(
        [ct.NT_SIGN.get(nt_of.get(int(r), "UNK"), 1.0) for r in conn["pre_root_id"]],
        dtype=np.float32,
    )
    w = conn["syn_count"].to_numpy(dtype=np.float32) * MV_PER_SYNAPSE * signs

    excit = float((w > 0).mean())
    print(f"  {excit:.1%} excitatory, {1 - excit:.1%} inhibitory")

    # Build CSR keyed by presynaptic neuron.
    order = np.argsort(pre_idx, kind="stable")
    pre_sorted, post_sorted, w_sorted = pre_idx[order], post_idx[order], w[order]
    counts = np.bincount(pre_sorted, minlength=n)
    indptr = np.zeros(n + 1, dtype=np.uint32)
    np.cumsum(counts, out=indptr[1:])

    # Motor map: which indices does the client read, and on which side.
    motor = defaultdict(lambda: {"left": [], "right": [], "other": []})
    for i, row in enumerate(sel.itertuples()):
        role = ct.DESCENDING.get(row.type)
        if role is None:
            continue
        side = str(row.side).lower()
        bucket = "left" if side.startswith("l") else "right" if side.startswith("r") else "other"
        motor[role][bucket].append(i)

    # Sensory map: where the tiled optic lobe delivers its output, and where
    # visual input reaches the ring attractor.
    sensory = defaultdict(list)
    for i, row in enumerate(sel.itertuples()):
        if row.type in ct.LOOMING or row.type in ct.LOBULA_PLATE_TANGENTIAL:
            sensory[row.type].append(i)
        elif row.type.startswith("ER"):
            sensory["ring_visual"].append(i)

    meta = {
        "version": "flywire-783",
        "source": "https://codex.flywire.ai  (CC-BY-4.0)",
        "min_synapses": args.min_synapses,
        "mv_per_synapse": MV_PER_SYNAPSE,
        "neurons": [
            {"id": int(r.root_id), "type": str(r.type), "side": str(r.side),
             "nt": str(r.nt_type), "block": str(r.block)}
            for r in sel.itertuples()
        ],
        "tonic": {"EPG": 4.5},   # see make_stub_pack.py for how this was set
        "motor": {k: dict(v) for k, v in motor.items()},
        "sensory": {k: v for k, v in sensory.items()},
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

    stats = write_pack(args.out, indptr, post_sorted, w_sorted, meta)
    print(f"\nwrote {args.out}")
    print(f"  {stats['neurons']:,} neurons, {stats['edges']:,} edges, "
          f"{stats['bytes'] / 1e6:.1f} MB")

    print("\nmotor outputs found:")
    for role in sorted(motor):
        b = motor[role]
        print(f"  {role:16s} L={len(b['left']):3d} R={len(b['right']):3d} "
              f"other={len(b['other']):3d}")

    if not motor.get("turn_fast", {}).get("left"):
        print("\n  WARNING: no left DNa02 found. Steering will not work. Check that "
              "your annotation file has a `side` column.")


if __name__ == "__main__":
    main()
