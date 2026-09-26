package ai.flybrain;

import java.util.concurrent.atomic.AtomicReference;

/**
 * The brain, running in real time, with a sensory front end and a motor
 * read-out. This is the part every client shares: the macOS overlay and the
 * web page both talk to one of these and neither knows how a neuron works.
 *
 * <p>The contract is small on purpose:
 * <ul>
 *   <li>{@link #setThreat} -- something is at this bearing and this angular
 *       size. The service turns that into drive on the looming detectors.</li>
 *   <li>{@link #snapshot} -- what the descending neurons are saying right now.</li>
 * </ul>
 *
 * <p>Everything in between is the circuit. No behaviour rules live here: if the
 * fly turns away from your cursor it is because LPLC2 on one side drove the
 * contralateral DNa02 harder, not because a rule said to.
 */
public final class BrainService implements Runnable {

    /** What a client is told each tick. Rates are in Hz. */
    public record MotorState(
            long tickMs,
            float turn,          // right minus left DNa02; positive turns right
            float forward,
            float escapeRun,
            float escapeJump,
            float stop,
            float headingRad,    // the fly's own heading estimate, from the ring
            float headingConfidence,
            int spikesPerSecond) {

        public String toJson() {
            return new Json.Writer().open()
                    .put("t", tickMs)
                    .put("turn", turn)
                    .put("forward", forward)
                    .put("escapeRun", escapeRun)
                    .put("escapeJump", escapeJump)
                    .put("stop", stop)
                    .put("heading", headingRad)
                    .put("headingConfidence", headingConfidence)
                    .put("spikeRate", spikesPerSecond)
                    .close().toString();
        }
    }

    private final LifBrain brain;
    private final CircuitPack pack;
    private final int[] lplc2;
    private final int[] lc4;
    private final int[] penLeft;
    private final int[] penRight;

    private final AtomicReference<MotorState> latest = new AtomicReference<>();

    // Threat state, written by clients, read by the loop.
    private volatile float threatBearing;    // radians, fly-centric; 0 is straight ahead
    private volatile float threatSize;       // arbitrary angular units
    private volatile boolean threatPresent;
    private float previousSize;

    private volatile boolean running = true;
    private volatile float turnFeedback;     // the body telling the brain it turned

    /** How broadly one looming cell's receptive field spreads, in radians squared. */
    private static final float RF_WIDTH = 1.30f;
    private static final int SECTORS = 16;

    public BrainService(CircuitPack pack, long seed) {
        this.pack = pack;
        this.brain = new LifBrain(pack, seed);
        this.lplc2 = pack.sensoryIndices("LPLC2");
        this.lc4 = pack.sensoryIndices("LC4");
        this.penLeft = pack.byType("PEN_a", "left");
        this.penRight = pack.byType("PEN_a", "right");
        brain.warmup(3, SECTORS);
    }

    public LifBrain brain() { return brain; }
    public MotorState snapshot() { return latest.get(); }
    public void stop() { running = false; }

    /**
     * Tell the brain something is out there.
     *
     * @param bearingRad where it is relative to the fly's own heading
     * @param angularSize how big it looks; the detectors care far more about
     *                    how fast this is growing than how large it is
     */
    public void setThreat(float bearingRad, float angularSize) {
        this.threatBearing = bearingRad;
        this.threatSize = angularSize;
        this.threatPresent = true;
    }

    public void clearThreat() {
        this.threatPresent = false;
        this.previousSize = 0f;
    }

    /**
     * Report that the body actually rotated. This closes the loop: the heading
     * bump is only correct because turning feeds back into the PENs, the same
     * way a real fly's proprioceptors and optic flow update its estimate.
     */
    public void reportTurn(float radiansPerSecond) {
        this.turnFeedback = radiansPerSecond;
    }

    // -- the loop ------------------------------------------------------------

    @Override
    public void run() {
        long startNanos = System.nanoTime();
        long tick = 0;
        float spikeRateEma = 0f;

        while (running) {
            // Catch up to wall-clock. One step is one millisecond of brain time;
            // if we fall behind we run extra steps rather than slow the fly down.
            long dueMs = (System.nanoTime() - startNanos) / 1_000_000L;
            int behind = (int) Math.min(50, dueMs - tick);

            if (behind <= 0) {
                sleepQuietly();
                continue;
            }

            int fired = 0;
            for (int i = 0; i < behind; i++) {
                applySenses();
                fired += brain.step();
                tick++;
            }

            spikeRateEma = spikeRateEma * 0.9f + (fired * 1000f / behind) * 0.1f;

            float[] bump = brain.bump();
            latest.set(new MotorState(
                    tick,
                    brain.turnCommand(),
                    brain.forwardCommand(),
                    brain.escapeRunRate(),
                    brain.escapeJumpRate(),
                    brain.stopRate(),
                    bump[0], bump[1],
                    Math.round(spikeRateEma)));
        }
    }

    private void applySenses() {
        // Turning feeds the PENs, which walk the heading bump around.
        float omega = turnFeedback;
        if (Math.abs(omega) > 0.01f) {
            float drive = Math.min(9f, Math.abs(omega) * 3.2f);
            brain.inject(omega > 0 ? penRight : penLeft, drive);
        }

        if (!threatPresent || lplc2.length == 0) {
            previousSize = 0f;
            return;
        }

        float size = threatSize;
        float expansion = size - previousSize;
        previousSize = size;

        // Size counts for a little, rate of expansion for a lot. That ratio is
        // what separates an alarming approach from a large stationary object.
        float drive = Math.max(0f, size * 0.30f + Math.max(0f, expansion) * 30f);
        if (drive < 0.4f) return;

        int perSector = Math.max(1, lplc2.length / SECTORS);
        int lc4PerSector = lc4.length == 0 ? 0 : Math.max(1, lc4.length / SECTORS);

        for (int k = 0; k < SECTORS; k++) {
            float sectorAngle = (k / (float) SECTORS) * 2f * (float) Math.PI - (float) Math.PI;
            float d = wrap(sectorAngle - threatBearing);
            float w = (float) Math.exp(-(d * d) / RF_WIDTH);
            if (w < 0.02f) continue;

            float mv = drive * w;
            for (int j = 0; j < perSector; j++) {
                int idx = k * perSector + j;
                if (idx < lplc2.length) brain.inject(lplc2[idx], mv);
            }
            for (int j = 0; j < lc4PerSector; j++) {
                int idx = k * lc4PerSector + j;
                if (idx < lc4.length) brain.inject(lc4[idx], mv * 0.7f);
            }
        }
    }

    private static float wrap(float a) {
        while (a < -Math.PI) a += 2 * Math.PI;
        while (a > Math.PI) a -= 2 * Math.PI;
        return Math.abs(a);
    }

    private static void sleepQuietly() {
        try {
            Thread.sleep(0, 200_000);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }
}
