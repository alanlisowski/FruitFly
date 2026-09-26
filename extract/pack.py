"""
The .fbp ("fly brain pack") format.

A circuit, flattened into something a runtime can mmap and start stepping in
under a millisecond. No JSON parsing of 300k edges at startup, no graph library
dependency in the service.

Layout, all little-endian:

    magic        4 bytes   "FBP1"
    n_neurons    uint32
    n_edges      uint32
    meta_len     uint32
    meta         meta_len bytes of UTF-8 JSON
    indptr       (n_neurons + 1) x uint32     CSR row pointers, by PREsynaptic neuron
    indices      n_edges x uint32             POSTsynaptic neuron index
    weights      n_edges x float32            signed millivolts delivered per spike

CSR is keyed by presynaptic neuron on purpose. The simulation is event-driven:
each millisecond we have a small list of neurons that spiked, and for each one
we want its outgoing edges contiguous in memory. That is exactly one CSR row.

The meta JSON carries everything non-numeric:

    {
      "version": "flywire-783",
      "neurons": [{"id": 720575940..., "type": "EPG", "side": "left",
                   "nt": "ACH", "block": "central_complex"}, ...],
      "motor": {"turn_fast": {"left": [12, 13], "right": [14, 15]}, ...},
      "sensory": {"R1": [...], ...},
      "params": {...}
    }
"""

import json
import struct
import numpy as np

MAGIC = b"FBP1"


def write_pack(path, indptr, indices, weights, meta):
    indptr = np.asarray(indptr, dtype="<u4")
    indices = np.asarray(indices, dtype="<u4")
    weights = np.asarray(weights, dtype="<f4")

    n_neurons = len(indptr) - 1
    n_edges = len(indices)
    if len(weights) != n_edges:
        raise ValueError(f"weights ({len(weights)}) and indices ({n_edges}) disagree")
    if indptr[-1] != n_edges:
        raise ValueError(f"indptr ends at {indptr[-1]}, expected {n_edges}")

    blob = json.dumps(meta, separators=(",", ":")).encode("utf-8")

    with open(path, "wb") as fh:
        fh.write(MAGIC)
        fh.write(struct.pack("<III", n_neurons, n_edges, len(blob)))
        fh.write(blob)
        fh.write(indptr.tobytes())
        fh.write(indices.tobytes())
        fh.write(weights.tobytes())

    return {"neurons": n_neurons, "edges": n_edges, "bytes": 16 + len(blob) + 4 * (n_neurons + 1) + 8 * n_edges}


def read_pack(path):
    with open(path, "rb") as fh:
        raw = fh.read()

    if raw[:4] != MAGIC:
        raise ValueError(f"not an .fbp file (magic was {raw[:4]!r})")

    n_neurons, n_edges, meta_len = struct.unpack_from("<III", raw, 4)
    off = 16
    meta = json.loads(raw[off:off + meta_len].decode("utf-8"))
    off += meta_len

    indptr = np.frombuffer(raw, dtype="<u4", count=n_neurons + 1, offset=off)
    off += 4 * (n_neurons + 1)
    indices = np.frombuffer(raw, dtype="<u4", count=n_edges, offset=off)
    off += 4 * n_edges
    weights = np.frombuffer(raw, dtype="<f4", count=n_edges, offset=off)

    return indptr, indices, weights, meta
