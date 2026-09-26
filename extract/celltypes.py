"""
Which parts of the fly brain we actually pull out of FlyWire.

The whole connectome is ~139k neurons. We do not need all of it to make a fly
walk around a screen. We need four functional blocks, and FlyWire's cell-type
annotations let us name them exactly.

Every name below is a real annotated cell type in the FlyWire v783 release
(column `cell_type` or `hemibrain_type` in Supplemental_file1_neuron_annotations.tsv).
Nothing here is invented; if a type is missing from your annotation file the
extractor will tell you rather than silently skipping it.
"""

# ---------------------------------------------------------------------------
# 1. VISION -- the columnar optic lobe.
#
# The optic lobe is ~35k neurons per side, which is far too many to simulate
# per-neuron at 1 kHz alongside everything else. It is also the most repetitive
# structure in the brain: the same ~65-cell-type motif tiled across ~750-800
# retinotopic columns (one per ommatidium).
#
# So we do what flyvis (Lappalainen et al. 2024) does: extract the *canonical
# column* -- the average connectivity between cell types within a column, plus
# the offsets to neighbouring columns -- and then tile it over a reduced
# retinotopic grid at runtime. The wiring is real; the tiling is the
# approximation, and it is the same approximation the published models make.
# ---------------------------------------------------------------------------

# Photoreceptors. R1-R6 are the motion/luminance channel, R7/R8 are colour.
# We take R1-R6 only; our fly is reacting to brightness and movement.
PHOTORECEPTORS = ["R1", "R2", "R3", "R4", "R5", "R6"]

# Lamina monopolar cells: the first processing stage. L1 feeds the ON pathway,
# L2/L3 feed the OFF pathway. L4/L5 do lateral work.
LAMINA = ["L1", "L2", "L3", "L4", "L5"]

# Medulla intrinsic + transmedullary cells. These are the delay lines and
# non-delayed arms that make up the elementary motion detector.
MEDULLA = [
    "Mi1", "Mi4", "Mi9",          # ON pathway, feeds T4
    "Tm1", "Tm2", "Tm4", "Tm9",   # OFF pathway, feeds T5
    "Tm3",                        # ON, fast arm
    "C2", "C3",                   # centrifugal feedback
    "CT1",                        # shared ON/OFF inhibition
]

# The motion detectors themselves. Four subtypes each, one per cardinal
# direction (a/b/c/d = front-to-back, back-to-front, upward, downward).
# T4 = ON motion, T5 = OFF motion. This is where direction selectivity is born.
T4T5 = ["T4a", "T4b", "T4c", "T4d", "T5a", "T5b", "T5c", "T5d"]

# Wide-field optic-flow integrators in the lobula plate. These pool T4/T5 over
# large parts of the visual field and are what actually drives turning.
# HS = horizontal system, VS = vertical system.
LOBULA_PLATE_TANGENTIAL = [
    "HSN", "HSE", "HSS",                    # horizontal, north/equatorial/south
    "VS1", "VS2", "VS3", "VS4", "VS5",      # vertical
    "VS6", "VS7", "VS8", "VS9", "VS10",
    "H1", "H2",                             # contralateral horizontal
]

OPTIC_LOBE = PHOTORECEPTORS + LAMINA + MEDULLA + T4T5 + LOBULA_PLATE_TANGENTIAL

# ---------------------------------------------------------------------------
# 2. THREAT -- lobula columnar cells that detect looming.
#
# These are the cells that make a real fly bolt when you reach for it. LPLC2 is
# the classic loom detector: it pools T4/T5 radially so that an object expanding
# symmetrically about its centre drives it hard, while translating objects
# largely cancel. LC4 encodes angular velocity of the expansion.
#
# Together they feed the giant fibre (DNp01) and DNp09. This is the pathway that
# will make the fly run from the cursor.
# ---------------------------------------------------------------------------
LOOMING = [
    "LPLC1", "LPLC2", "LPLC4",
    "LC4", "LC6", "LC16", "LC22",
]

# ---------------------------------------------------------------------------
# 3. HEADING + STEERING -- the central complex.
#
# The ring attractor. EPG neurons hold a single bump of activity whose angular
# position around the ellipsoid body *is* the fly's internal heading estimate.
# PEN neurons rotate that bump when the fly turns (they are the integrator);
# Delta7 provides the global inhibition that keeps it a single bump.
#
# PFL3 then compares the heading bump against a goal direction and emits an
# asymmetric left/right signal -- this is the actual steering command. PFL2
# controls forward drive.
#
# This block is the reason the fly will hold a course instead of doing a random
# walk, and it is small enough (~600 neurons) to simulate every one.
# ---------------------------------------------------------------------------
CENTRAL_COMPLEX = [
    "EPG", "EPGt",          # heading bump (ellipsoid body <-> protocerebral bridge)
    "PEN_a", "PEN_b",       # angular velocity integration, rotates the bump
    "PEG",                  # bump maintenance
    "Delta7",               # global inhibition, enforces a single bump
    "ER1_a", "ER1_b", "ER2_a", "ER2_b", "ER2_c", "ER2_d",   # ring neurons: visual input to the bump
    "ER3a_a", "ER3d_a", "ER3d_b", "ER3d_c", "ER3d_d",
    "ER4d", "ER4m",
    "ExR1", "ExR2", "ExR3", "ExR4", "ExR5", "ExR6",         # neuromodulatory / state
    "PFL1", "PFL2", "PFL3",                                 # heading -> steering readout
    "PFR_a", "PFR_b",
    "FC1A", "FC1B", "FC1C", "FC2A", "FC2B", "FC2C",         # goal / vector memory
    "FB1A", "FB2A", "FB4X",
    "hDeltaA", "hDeltaB", "hDeltaC", "hDeltaJ", "hDeltaK",  # fan-shaped body horizontal
    "vDeltaA_a", "vDeltaB", "vDeltaC",
]

# ---------------------------------------------------------------------------
# 4. OUTPUT -- descending neurons.
#
# ~1300 neurons carry everything the brain decides down to the ventral nerve
# cord, where the legs live. We only need the locomotor ones. These are our
# motor outputs: the entire behaviour of the desktop fly is read off their
# firing rates.
# ---------------------------------------------------------------------------
DESCENDING = {
    # --- steering: the see-saw pair ---
    # Rotational velocity is linear in the right-minus-left firing rate
    # difference of DNa02 (Rayshubskiy et al. / eLife 102230). DNa01 is the
    # slower, sustained partner. DNa02 leads a turn by ~150 ms.
    "DNa01": "turn_sustained",
    "DNa02": "turn_fast",
    "DNa03": "turn_amplify",
    "DNa04": "turn",
    # --- forward drive ---
    "DNp09": "stop",          # bilateral activation freezes the fly
    "DNb02": "forward",
    "DNg13": "forward",
    "BDN2": "forward_fast",   # bilateral descending neuron 2, drives fast walking
    "DNg12": "backward",
    "MDN": "backward",        # moonwalker descending neuron -- literally reverses
    # --- escape ---
    "DNp01": "escape_jump",   # the giant fibre. One spike = takeoff.
    "DNp02": "escape_run",
    "DNp04": "escape",
    "DNp06": "escape",
    "DNp11": "escape",
    # --- grooming / idle behaviours, for flavour ---
    "DNg11": "groom",
    "DNg8": "groom",
}

# Roles we hand to the runtime. The client never needs to know a cell type,
# only these.
MOTOR_ROLES = sorted(set(DESCENDING.values()))


def all_types():
    """Every cell type we want, as a flat set."""
    return set(OPTIC_LOBE) | set(LOOMING) | set(CENTRAL_COMPLEX) | set(DESCENDING)


# ---------------------------------------------------------------------------
# Neurotransmitter -> sign.
#
# FlyWire ships a per-synapse neurotransmitter prediction (`nt_type`). We
# collapse it to a sign: excitatory or inhibitory. This is the standard
# simplification -- the alternative is modelling receptor kinetics per synapse,
# which nobody does at whole-brain scale.
#
# Glutamate is the awkward one: in Drosophila it is usually inhibitory
# (GluCl-alpha), unlike in vertebrates. Getting this backwards is the single
# most common way to make a connectome simulation produce garbage.
# ---------------------------------------------------------------------------
NT_SIGN = {
    "ACH": +1.0,    # acetylcholine -- the main excitatory transmitter
    "GLUT": -1.0,   # glutamate -- inhibitory in flies
    "GABA": -1.0,
    "SER": +0.5,    # serotonin, octopamine, dopamine are modulatory; we give
    "OCT": +0.5,    # them a weak excitatory sign rather than dropping them.
    "DA": +0.5,
    "UNK": +1.0,    # unknown: assume excitatory (ACh is ~50% of all synapses)
}
