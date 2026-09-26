# flybrain

A fly that walks around your screen, driven by a spiking simulation of real
*Drosophila* circuitry rather than by animation curves.

The brain runs as a service. The desktop overlay and the web page are both thin
clients: they send it what the fly can see and read back what its descending
neurons are saying. Neither knows how a neuron works.

```
  FlyWire v783                 brain.fbp              BrainService (Java)
  connections + annotations →  ~3k neurons       →    LIF @ 1 kHz
  (CC-BY-4.0, ~250 MB)         ~300k edges            event-driven CSR
                               2.6 MB                        │
                                                    ws://localhost:8787
                                                       │           │
                                            macOS overlay      web page
                                            (ScreenCaptureKit) (canvas)
```

---

## Status

| Piece | State |
|---|---|
| Pack format + loader (Python, Java) | done |
| LIF core, Python reference | done, 46× real time |
| LIF core, Java | done, 643× real time on the stub; 14× at 3k neurons |
| Behavioural test suite | done, 4 checks, passing in both languages |
| Extraction from real FlyWire tables | written, **not yet run against the real download** |
| Web client | done — see `clients/web/index.html` |
| WebSocket service | done, dependency-free |
| macOS overlay | not started — see `clients/macos/NOTES.md` |
| Optic lobe (tiled columnar vision) | not started |

The circuit currently shipping is a **stub**: the right cell types, the right
transmitter signs, the right topology, but connection strengths set by hand from
published circuit diagrams and then tuned, not measured synapse counts. Running
`extract_circuit.py` against the real tables replaces the file and nothing else.

---

## Prior art, honestly

[DesktopFly](https://github.com/DenisSergeevitch/desktop-fly) already does a
version of this: macOS, FlyWire connectome, a 668-neuron circuit at 1 kHz, flees
your cursor. It is worth looking at before you build anything.

Where this project differs, and what would make it worth finishing:

- **The fly sees your actual screen.** DesktopFly reacts to cursor position.
  Feeding a downsampled screen capture into a tiled columnar optic lobe means
  the fly reacts to your windows, your text, your scrolling — the thing that
  makes it feel alive rather than scripted.
- **The brain is a service, not a library.** One simulation, several clients,
  a wire protocol you can point anything at.
- **A heading system, not just a reflex.** The ring attractor is what makes the
  fly hold a course and resume it after a scare, instead of doing a random walk
  between startles.

---

## Running it

Nothing here needs the connectome download to start.

```bash
# 1. Build the stub circuit
python3 extract/make_stub_pack.py --out data/brain_stub.fbp

# 2. Check it behaves like a fly brain
python3 extract/test_circuit.py

# 3. Same checks, Java
cd core && javac -d out $(find src -name '*.java') && cd ..
java -cp core/out ai.flybrain.Main verify data/brain_stub.fbp
java -cp core/out ai.flybrain.Main bench  data/brain_stub.fbp

# 4. Run the service
java -cp core/out ai.flybrain.Main serve data/brain_stub.fbp 8787

# 5. Open clients/web/index.html for the self-contained demo
```

### With the real connectome

Two files, both from [Codex](https://codex.flywire.ai) (CC-BY-4.0, cite Dorkenwald
et al. and Schlegel et al.):

- `Supplemental_file1_neuron_annotations.tsv` — cell types, sides,
  neurotransmitters (~25 MB, also on
  [GitHub](https://github.com/flyconnectome/flywire_annotations))
- `connections_princeton` — the neuron-to-neuron edge table (~250 MB gzipped).
  The 9.5 GB per-synapse file on Zenodo is not needed; it carries coordinates we
  never use.

```bash
python3 extract/extract_circuit.py \
    --annotations data/annotations.tsv \
    --connections data/connections.csv.gz \
    --out data/brain.fbp
```

Then point everything at `brain.fbp` instead. Run `verify` first — if the
behavioural checks fail on the real pack, the extraction is wrong, not the idea.

---

## Layout

```
extract/
  celltypes.py        which cells we pull out, and why. Read this first.
  extract_circuit.py  FlyWire tables → brain.fbp
  make_stub_pack.py   hand-built stand-in, so the stack runs today
  pack.py             the .fbp format
  sim.py              numpy reference simulator
  test_circuit.py     four behavioural checks
  tune_ring.py        parameter sweeps
  diagnose.py         population firing rates, for when something is silent
core/
  CircuitPack.java    loader
  LifBrain.java       the simulation
  BrainService.java   real-time loop, sensory encoding, motor readout
  WsServer.java       WebSocket, no dependencies
  Json.java           small reader/writer
  Main.java           serve | bench | verify
clients/
  web/index.html      self-contained demo, runs the same model in JS
  macos/NOTES.md      what the overlay needs
```

---

## The model

Leaky integrate-and-fire, the parameters used in the published whole-brain
FlyWire simulation:

| | |
|---|---|
| Membrane time constant | 20 ms |
| Synaptic time constant | 5 ms |
| Resting / threshold / reset | −52 / −45 / −52 mV |
| Refractory period | 2.2 ms |
| Weight per synapse | 0.275 mV, signed by the presynaptic transmitter |
| Step | 1 ms |

Glutamate is inhibitory in flies, unlike in vertebrates. Getting that sign
backwards is the most efficient way to produce a connectome simulation that
looks plausible and means nothing.

### Four blocks

**Vision** — the optic lobe is ~35k neurons per side and the most repetitive
structure in the brain: one ~65-cell-type motif tiled across ~750 columns. The
plan is to extract the canonical column and tile it at runtime, the way
[flyvis](https://github.com/TuragaLab/flyvis) does. Not built yet.

**Threat** — LPLC2 and LC4. LPLC2 pools motion radially, so something expanding
around a point drives it hard while something merely sliding past largely
cancels. Targets: DNp02 (run) and DNp01, the giant fibre (jump).

**Heading** — the ring attractor. EPG cells hold one bump whose angular position
is the fly's heading estimate; PEN cells rotate it; Delta7 supplies the global
inhibition that keeps it a single bump. PFL3 reads it out against a goal.

**Output** — descending neurons. Turn rate is the right-minus-left firing rate
difference of DNa02; forward drive comes off PFL2 → BDN2.

---

## Things that turned out to matter

**A single spike does almost nothing.** The membrane integrates with a 20 ms
time constant while the synapse decays with a 5 ms one, so a 9 mV spike moves
the membrane under 2 mV — the synapse has mostly decayed before the membrane has
finished responding. Sustained input is what fires a neuron:
`g ≈ N · w · rate · 0.005`. Every early tuning failure here traced back to
ignoring that and reasoning from weights directly.

**The ring attractor's working window is narrow.** Sweeping recurrent excitation
against Delta7 inhibition, the bump either dies within a second or floods the
whole ring. The band between them is a few synapses wide. It also needs a tonic
background drive — 4.5 mV — or it decays regardless; the real ellipsoid body
receives steady input. Tuned: a 5-wedge bump near 160 Hz drifting about 1°/s.

**A settled bump resists being moved.** Rotation with the same PEN drive fell
from +41° to +10° per 500 ms purely from letting the bump settle 250 ms longer.
With only 16 wedges the attractor is lumpy — the bump prefers sitting *on* a
wedge — so single-trial rotation tests are noisy. The test now averages six
seeds and checks that the two sides disagree in direction, which is what
steering actually requires. More wedges, or the real connectome's richer
wiring, should smooth this.

**Performance is not the constraint.** Event-driven propagation over a CSR
adjacency means cost scales with spikes, not synapses. Measured: 643× real time
on the stub, and 14× real time on a 3,000-neuron / 300,000-edge pack driven to
33% activity — roughly ten times busier than a real fly brain. The bottleneck
will be the screen capture and the optic lobe, not the spiking.

---

## Known limitations

- Connection strengths are hand-set, not measured. Everything above is a
  statement about the architecture, not about the fly.
- The ring is 16 wedges against ~46 real EPG neurons; it pins.
- The escape latch (420 ms) lives in the body model, not the brain. In the
  animal that burst hands off to the ventral nerve cord, which we do not have.
  It is the one place behaviour is not read straight off a firing rate, and it
  is marked as such in the code.
- No optic lobe yet, so the fly cannot see anything except a threat position
  handed to it by the client.
- The `keep_optic_lobe` path in the extractor is untested at scale.

---

## Data and citation

FlyWire data is CC-BY-4.0. If any of this goes public, cite the connectome
papers (Dorkenwald et al. 2024 for the dataset, Schlegel et al. 2024 for the
annotations) and the whole-brain LIF model whose neuron parameters this uses
(Shiu et al. 2024). The code here is yours; the connectome is theirs.
