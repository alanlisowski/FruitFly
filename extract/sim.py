#!/usr/bin/env python3
"""
Reference LIF simulator. Numpy, readable, slow-ish.

This exists to check that a pack behaves before you port anything. The Java
service implements exactly this and must match it step for step.

The neuron model is the one Shiu et al. used for the whole-brain FlyWire
simulation:

    dv/dt = (v_rest - v + g) / tau_membrane     (held at reset while refractory)
    dg/dt = -g / tau_synapse

v is membrane voltage, g is an aggregate synaptic conductance in voltage units.
A presynaptic spike adds its edge weight straight into the postsynaptic g,
which then decays with a 5 ms time constant -- so a single spike gives a small
kick that fades, and coincident spikes sum.
"""

import numpy as np
from pack import read_pack


class Brain:
    def __init__(self, path, seed=0, noise_mv=0.0):
        self.indptr, self.indices, self.weights, self.meta = read_pack(path)
        p = self.meta["params"]

        self.n = len(self.indptr) - 1
        self.dt = p["dt_ms"]
        self.v_rest = p["v_rest_mv"]
        self.v_th = p["v_threshold_mv"]
        self.v_reset = p["v_reset_mv"]
        self.decay_v = np.exp(-self.dt / p["tau_membrane_ms"])
        self.decay_g = np.exp(-self.dt / p["tau_synapse_ms"])
        self.refractory_steps = int(round(p["refractory_ms"] / self.dt))

        self.v = np.full(self.n, self.v_rest, dtype=np.float32)
        self.g = np.zeros(self.n, dtype=np.float32)
        self.refrac = np.zeros(self.n, dtype=np.int16)
        self.ext = np.zeros(self.n, dtype=np.float32)   # injected current, mV

        self.noise_mv = noise_mv
        self.rng = np.random.default_rng(seed)

        # Exponentially smoothed firing rate per neuron, for readout.
        self.rate = np.zeros(self.n, dtype=np.float32)
        self.rate_decay = np.exp(-self.dt / 50.0)   # 50 ms window

        self.types = [x["type"] for x in self.meta["neurons"]]

        # Tonic background drive, resolved once from cell type to neuron index.
        self.tonic = np.zeros(self.n, dtype=np.float32)
        for type_, mv in self.meta.get("tonic", {}).items():
            for i, nrn in enumerate(self.meta["neurons"]):
                if nrn["type"] == type_:
                    self.tonic[i] = mv
        self.motor = self.meta["motor"]
        self.sensory = self.meta["sensory"]

    def inject(self, indices, mv):
        """Add depolarising current to a set of neurons for the next step."""
        if len(indices):
            self.ext[np.asarray(indices, dtype=np.int64)] += mv

    def step(self):
        # Membrane integrates toward v_rest + g.
        target = self.v_rest + self.g + self.ext + self.tonic
        self.v = target + (self.v - target) * self.decay_v
        if self.noise_mv:
            self.v += self.rng.normal(0.0, self.noise_mv, self.n).astype(np.float32)

        # Refractory neurons are pinned at reset.
        held = self.refrac > 0
        self.v[held] = self.v_reset
        self.refrac[held] -= 1

        spiked = np.flatnonzero((self.v >= self.v_th) & ~held)

        self.v[spiked] = self.v_reset
        self.refrac[spiked] = self.refractory_steps

        # Synaptic conductance decays, then takes this step's spikes.
        self.g *= self.decay_g
        if spiked.size:
            # Event-driven: gather only the outgoing edges of neurons that fired.
            starts = self.indptr[spiked]
            ends = self.indptr[spiked + 1]
            total = int((ends - starts).sum())
            if total:
                tgt = np.empty(total, dtype=np.int64)
                val = np.empty(total, dtype=np.float32)
                at = 0
                for s, e in zip(starts, ends):
                    k = e - s
                    if k:
                        tgt[at:at + k] = self.indices[s:e]
                        val[at:at + k] = self.weights[s:e]
                        at += k
                np.add.at(self.g, tgt, val)

        # Firing rate estimate, in Hz.
        self.rate *= self.rate_decay
        if spiked.size:
            self.rate[spiked] += (1.0 - self.rate_decay) * (1000.0 / self.dt)

        self.ext[:] = 0.0
        return spiked

    # -- readout -----------------------------------------------------------

    def role_rate(self, role, side):
        idx = self.motor.get(role, {}).get(side, [])
        if not idx:
            return 0.0
        return float(self.rate[np.asarray(idx, dtype=np.int64)].mean())

    def turn_command(self):
        """Right minus left DNa02 rate. Positive = turn right."""
        fast = self.role_rate("turn_fast", "right") - self.role_rate("turn_fast", "left")
        slow = self.role_rate("turn_sustained", "right") - self.role_rate("turn_sustained", "left")
        return fast + 0.4 * slow

    def forward_command(self):
        return (self.role_rate("forward_fast", "other")
                + 0.5 * (self.role_rate("forward", "left") + self.role_rate("forward", "right")))

    def bump_position(self):
        """
        Where is the heading bump? Circular mean of EPG rates around the ring.
        Returns (angle_radians, magnitude). Magnitude near 0 means no bump.
        """
        idx = self.sensory.get("EPG")
        if not idx:
            return 0.0, 0.0
        r = self.rate[np.asarray(idx, dtype=np.int64)]
        if r.sum() <= 0:
            return 0.0, 0.0
        theta = np.linspace(0, 2 * np.pi, len(r), endpoint=False)
        x = float((r * np.cos(theta)).sum())
        y = float((r * np.sin(theta)).sum())
        return float(np.arctan2(y, x)), float(np.hypot(x, y) / r.sum())
