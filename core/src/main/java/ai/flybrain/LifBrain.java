package ai.flybrain;

/**
 * The simulation. Leaky integrate-and-fire, one millisecond at a time.
 *
 * <pre>
 *   dv/dt = (vRest - v + g) / tauMembrane     (held at reset while refractory)
 *   dg/dt = -g / tauSynapse
 * </pre>
 *
 * <p>{@code v} is membrane voltage; {@code g} is an aggregate synaptic
 * conductance carried in voltage units. A presynaptic spike adds its edge
 * weight straight into the target's {@code g}, which then decays.
 *
 * <p>The two time constants are 20 ms and 5 ms, and that gap has a consequence
 * worth stating plainly: a single 9 mV spike moves the membrane by under 2 mV,
 * because the synapse has largely decayed before the membrane has finished
 * responding. Neurons here fire on coincidence, not on any one strong input.
 * Tuning a circuit without accounting for it produces a brain that does
 * nothing, which is a confusing way to spend an afternoon.
 *
 * <p>Not thread-safe. One brain per thread; {@link BrainService} owns the loop.
 */
public final class LifBrain {

    private final CircuitPack pack;
    private final int n;

    private final float[] v;
    private final float[] g;
    private final float[] ext;      // injected current for the next step, mV
    private final float[] rate;     // exponentially smoothed firing rate, Hz
    private final short[] refractory;
    private final int[] spikeBuf;
    private int spikeCount;

    private final float vRest, vTh, vReset, decayV, decayG, rateDecay, rateKick;
    private final short refractorySteps;

    private float noiseMv = 0.30f;
    private long stepsRun;

    /** Deterministic noise: same seed, same trajectory. */
    private long rngState;

    public LifBrain(CircuitPack pack, long seed) {
        this.pack = pack;
        this.n = pack.neuronCount;

        CircuitPack.Params p = pack.params;
        this.vRest = (float) p.vRestMv();
        this.vTh = (float) p.vThresholdMv();
        this.vReset = (float) p.vResetMv();
        this.decayV = (float) Math.exp(-p.dtMs() / p.tauMembraneMs());
        this.decayG = (float) Math.exp(-p.dtMs() / p.tauSynapseMs());
        this.refractorySteps = (short) Math.round(p.refractoryMs() / p.dtMs());
        this.rateDecay = (float) Math.exp(-p.dtMs() / 50.0);
        this.rateKick = (float) ((1.0 - rateDecay) * (1000.0 / p.dtMs()));

        this.v = new float[n];
        this.g = new float[n];
        this.ext = new float[n];
        this.rate = new float[n];
        this.refractory = new short[n];
        this.spikeBuf = new int[n];
        this.rngState = seed == 0 ? 0x9E3779B97F4A7C15L : seed;

        java.util.Arrays.fill(v, vRest);
    }

    public void setNoiseMv(float mv) { this.noiseMv = mv; }

    public int neuronCount() { return n; }
    public long stepsRun() { return stepsRun; }
    public int lastSpikeCount() { return spikeCount; }
    public int[] lastSpikes() { return spikeBuf; }
    public CircuitPack pack() { return pack; }

    /** Depolarise a set of neurons on the next step only. */
    public void inject(int[] idx, float mv) {
        for (int i : idx) ext[i] += mv;
    }

    public void inject(int idx, float mv) { ext[idx] += mv; }

    /** Advance one millisecond. Returns the number of neurons that fired. */
    public int step() {
        final float[] v = this.v, g = this.g, ext = this.ext, tonic = pack.tonic;
        final short[] refractory = this.refractory;
        int ns = 0;

        for (int i = 0; i < n; i++) {
            if (refractory[i] > 0) {
                refractory[i]--;
                v[i] = vReset;
                ext[i] = 0f;
                continue;
            }
            float target = vRest + g[i] + ext[i] + tonic[i];
            v[i] = target + (v[i] - target) * decayV + nextNoise();
            ext[i] = 0f;
            if (v[i] >= vTh) {
                v[i] = vReset;
                refractory[i] = refractorySteps;
                spikeBuf[ns++] = i;
            }
        }

        for (int i = 0; i < n; i++) g[i] *= decayG;

        // Event-driven propagation. Only the rows of neurons that fired get
        // touched, which is why a 300k-edge circuit costs a few thousand
        // operations per millisecond rather than 300k.
        final int[] indptr = pack.indptr, indices = pack.indices;
        final float[] weights = pack.weights;
        for (int s = 0; s < ns; s++) {
            int pre = spikeBuf[s];
            int end = indptr[pre + 1];
            for (int e = indptr[pre]; e < end; e++) {
                g[indices[e]] += weights[e];
            }
        }

        for (int i = 0; i < n; i++) rate[i] *= rateDecay;
        for (int s = 0; s < ns; s++) rate[spikeBuf[s]] += rateKick;

        spikeCount = ns;
        stepsRun++;
        return ns;
    }

    /** xorshift64*, scaled to roughly uniform noise in [-noise/2, +noise/2]. */
    private float nextNoise() {
        if (noiseMv == 0f) return 0f;
        long x = rngState;
        x ^= x >>> 12; x ^= x << 25; x ^= x >>> 27;
        rngState = x;
        float u = ((x * 0x2545F4914F6CDD1DL) >>> 40) / (float) (1 << 24); // [0,1)
        return (u - 0.5f) * noiseMv;
    }

    // -- readout -------------------------------------------------------------

    public float rateOf(int idx) { return rate[idx]; }

    public float meanRate(int[] idx) {
        if (idx.length == 0) return 0f;
        float t = 0f;
        for (int i : idx) t += rate[i];
        return t / idx.length;
    }

    public float roleRate(String role, String side) {
        return meanRate(pack.motorIndices(role, side));
    }

    /** Right minus left DNa02, plus a weighted share of the slower DNa01 pair. */
    public float turnCommand() {
        return (roleRate("turn_fast", "right") - roleRate("turn_fast", "left"))
             + 0.4f * (roleRate("turn_sustained", "right") - roleRate("turn_sustained", "left"));
    }

    public float forwardCommand() {
        return roleRate("forward_fast", "other")
             + 0.5f * (roleRate("forward", "left") + roleRate("forward", "right"));
    }

    public float escapeRunRate() {
        return roleRate("escape_run", "left") + roleRate("escape_run", "right");
    }

    public float escapeJumpRate() {
        return roleRate("escape_jump", "left") + roleRate("escape_jump", "right");
    }

    public float stopRate() {
        return roleRate("stop", "left") + roleRate("stop", "right");
    }

    /**
     * Where the heading bump sits, as the circular mean of EPG firing rates.
     * Returns {angleRadians, magnitude}; a magnitude near zero means the ring
     * has no coherent bump and the heading estimate should not be trusted.
     */
    public float[] bump() {
        int[] epg = pack.sensoryIndices("EPG");
        if (epg.length == 0) return new float[] {0f, 0f};
        double x = 0, y = 0, sum = 0;
        for (int k = 0; k < epg.length; k++) {
            double theta = 2 * Math.PI * k / epg.length;
            double r = rate[epg[k]];
            x += r * Math.cos(theta);
            y += r * Math.sin(theta);
            sum += r;
        }
        if (sum <= 0) return new float[] {0f, 0f};
        return new float[] {(float) Math.atan2(y, x), (float) (Math.hypot(x, y) / sum)};
    }

    /**
     * Hold one wedge above threshold until recurrent excitation takes over,
     * then let the attractor settle. A single-step kick will not do it:
     * injected current is spent each step, and one spike barely moves a
     * membrane.
     */
    public void warmup(int wedge, int wedgeCount) {
        int[] epg = pack.sensoryIndices("EPG");
        if (epg.length == 0) return;
        int per = Math.max(1, epg.length / wedgeCount);
        int[] seed = new int[per];
        for (int k = 0; k < per; k++) seed[k] = epg[(wedge * per + k) % epg.length];

        for (int t = 0; t < 200; t++) { inject(seed, 16f); step(); }
        for (int t = 0; t < 400; t++) step();
    }
}
