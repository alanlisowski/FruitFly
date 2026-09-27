package ai.flybrain;

import java.nio.file.Path;

/**
 * Entry point. Four modes:
 *
 * <pre>
 *   java -cp out ai.flybrain.Main serve  data/brain.fbp [port]
 *   java -cp out ai.flybrain.Main bench  data/brain.fbp
 *   java -cp out ai.flybrain.Main verify data/brain.fbp
 *   java -cp out ai.flybrain.Main trace  data/brain_stub.fbp > app/tests/golden/stub_trace.txt
 * </pre>
 *
 * <p>{@code verify} runs the same four behavioural checks as the Python
 * reference. Both implementations claim to be the same model, so they had
 * better agree, and a check that only exists in one language is a check that
 * quietly stops being true.
 */
public final class Main {

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: Main <serve|bench|verify|trace> <pack.fbp> [port]");
            System.exit(2);
        }
        String mode = args[0];
        CircuitPack pack = CircuitPack.load(Path.of(args[1]));
        if (!mode.equals("trace")) {   // trace output is data, nothing else on stdout
            System.out.printf("loaded %s: %,d neurons, %,d edges%n",
                    args[1], pack.neuronCount, pack.edgeCount);
        }

        switch (mode) {
            case "serve" -> serve(pack, args.length > 2 ? Integer.parseInt(args[2]) : 8787);
            case "bench" -> bench(pack);
            case "verify" -> verify(pack);
            case "trace" -> trace(pack);
            default -> {
                System.err.println("unknown mode: " + mode);
                System.exit(2);
            }
        }
    }

    // ------------------------------------------------------------------------

    private static void serve(CircuitPack pack, int port) throws Exception {
        BrainService service = new BrainService(pack, 1);
        Thread.ofPlatform().name("fly-brain").daemon(false).start(service);

        try (WsServer ws = new WsServer(port, service)) {
            Thread.ofPlatform().name("fly-accept").daemon(true).start(ws::acceptLoop);
            System.out.println("brain listening on ws://localhost:" + ws.port());
            System.out.println("press ctrl-c to stop");

            // Clients render at screen rate; the brain runs far faster. Send
            // state at 60 Hz and let them interpolate between frames.
            while (true) {
                ws.broadcast();
                Thread.sleep(16);
            }
        }
    }

    private static void bench(CircuitPack pack) {
        LifBrain brain = new LifBrain(pack, 1);
        brain.warmup(3, 16);

        for (int i = 0; i < 50_000; i++) brain.step();   // let the JIT settle

        int steps = 500_000;
        long spikes = 0;
        long t0 = System.nanoTime();
        for (int i = 0; i < steps; i++) spikes += brain.step();
        double elapsed = (System.nanoTime() - t0) / 1e9;

        System.out.printf("%,d steps (%.0f s of brain time) in %.2f s wall%n",
                steps, steps / 1000.0, elapsed);
        System.out.printf("  %.2f us per 1 ms step  ->  %.0fx real time%n",
                elapsed / steps * 1e6, steps / elapsed / 1000.0);
        System.out.printf("  mean %.2f spikes/ms across %,d neurons (%.2f%% active)%n",
                spikes / (double) steps, pack.neuronCount,
                spikes / (double) steps / pack.neuronCount * 100);

        double edgesPerSpike = pack.edgeCount / (double) pack.neuronCount;
        System.out.printf("  ~%,.0f edge updates per ms%n",
                spikes / (double) steps * edgesPerSpike);
        System.out.printf("  headroom at 1 kHz: %.0fx%n", steps / elapsed / 1000.0);
    }

    /**
     * Golden trace for the Rust port (app/tests/brain.rs): noise off, warmup(3, 16), then
     * 3000 steps of a fixed protocol -- rest, left PEN drive, rest, right PEN drive, rest,
     * a looming ramp, rest -- printing every spike as "step neuron". The port must reproduce
     * it exactly. Change the protocol here and in the Rust test together.
     */
    private static void trace(CircuitPack pack) {
        LifBrain b = new LifBrain(pack, 1);
        b.setNoiseMv(0f);
        b.warmup(3, 16);
        int[] penL = pack.byType("PEN_a", "left"), penR = pack.byType("PEN_a", "right");
        int[] lplc2 = pack.sensoryIndices("LPLC2"), lc4 = pack.sensoryIndices("LC4");
        StringBuilder out = new StringBuilder();
        for (int t = 0; t < 3000; t++) {
            if (t >= 500 && t < 1000) b.inject(penL, 9f);
            if (t >= 1200 && t < 1700) b.inject(penR, 9f);
            if (t >= 2000 && t < 2300) {
                float drive = 16f * ((t - 2000) / 300f) * ((t - 2000) / 300f);
                b.inject(lplc2, drive);
                b.inject(lc4, drive * 0.7f);
            }
            int ns = b.step();
            int[] s = b.lastSpikes();
            for (int k = 0; k < ns; k++) out.append(t).append(' ').append(s[k]).append('\n');
        }
        System.out.print(out);
    }

    // ------------------------------------------------------------------------

    private static int failures = 0;

    private static void verify(CircuitPack pack) {
        System.out.println();
        bumpFormsAndHolds(pack);
        bumpRotates(pack);
        loomingTriggersEscape(pack);
        escapeIsDirectional(pack);
        System.out.printf("%n%d check(s) failed%n", failures);
        if (failures > 0) System.exit(1);
    }

    private static void check(String what, boolean ok, String detail) {
        System.out.printf("  %-44s %s%s%n", what, ok ? "PASS" : "FAIL",
                detail.isEmpty() ? "" : "  (" + detail + ")");
        if (!ok) failures++;
    }

    private static void bumpFormsAndHolds(CircuitPack pack) {
        System.out.println("bump forms and holds");
        LifBrain b = new LifBrain(pack, 1);
        b.warmup(3, 16);

        float[] before = b.bump();
        for (int i = 0; i < 600; i++) b.step();
        float[] after = b.bump();

        double drift = Math.toDegrees(Math.abs(Math.atan2(
                Math.sin(after[0] - before[0]), Math.cos(after[0] - before[0]))));

        check("coherent bump after 600 ms", after[1] > 0.25,
                String.format("magnitude %.3f", after[1]));
        check("drift under 60 deg", drift < 60, String.format("%.1f deg", drift));
    }

    /**
     * Drive one side's PENs and the bump should walk that way.
     *
     * <p>Judged across seeds deliberately. With 16 wedges the attractor is
     * lumpy: the bump prefers sitting on a wedge to sitting between two, so a
     * single trial can stick or jump two wedges at once. The claim being
     * tested is that the two sides push it opposite ways on average, which is
     * what steering actually requires.
     */
    private static void bumpRotates(CircuitPack pack) {
        System.out.println("bump rotates with PEN drive");
        int seeds = 6;
        double sumLeft = 0, sumRight = 0;
        int agree = 0;

        for (int seed = 1; seed <= seeds; seed++) {
            double l = penShift(pack, "left", seed);
            double r = penShift(pack, "right", seed);
            sumLeft += l;
            sumRight += r;
            if (Math.signum(l) != Math.signum(r)) agree++;
        }
        double meanLeft = sumLeft / seeds, meanRight = sumRight / seeds;

        check("left PEN drive walks the bump one way", meanLeft > 8,
                String.format("%+.1f deg / 500 ms", meanLeft));
        check("right PEN drive walks it the other", meanRight < -3,
                String.format("%+.1f deg / 500 ms", meanRight));
        check("sides disagree on direction", agree >= 5,
                agree + "/" + seeds + " seeds");
    }

    private static double penShift(CircuitPack pack, String side, long seed) {
        LifBrain b = new LifBrain(pack, seed);
        b.warmup(3, 16);
        float before = b.bump()[0];
        int[] pen = pack.byType("PEN_a", side);
        for (int i = 0; i < 500; i++) { b.inject(pen, 9f); b.step(); }
        float after = b.bump()[0];
        return Math.toDegrees(Math.atan2(Math.sin(after - before), Math.cos(after - before)));
    }

    private static void loomingTriggersEscape(CircuitPack pack) {
        System.out.println("looming triggers escape");
        LifBrain b = new LifBrain(pack, 3);
        b.warmup(3, 16);

        float baseline = b.escapeRunRate();
        int[] lplc2 = pack.sensoryIndices("LPLC2");
        int[] lc4 = pack.sensoryIndices("LC4");
        for (int t = 0; t < 300; t++) {
            float drive = 16f * (t / 300f) * (t / 300f);
            b.inject(lplc2, drive);
            b.inject(lc4, drive * 0.7f);
            b.step();
        }

        float run = b.escapeRunRate();
        float jump = b.escapeJumpRate();
        check("looming drives the escape-run pathway", run > baseline + 20,
                String.format("%.0f -> %.0f Hz", baseline, run));
        check("the giant fibre stays the harder trigger", run > jump,
                String.format("DNp02 %.0f Hz vs DNp01 %.0f Hz", run, jump));
    }

    private static void escapeIsDirectional(CircuitPack pack) {
        System.out.println("escape is directional");
        float left = loomFromSector(pack, 0, 8);
        float right = loomFromSector(pack, 8, 16);
        check("fly turns away from the threatened side", left > right,
                String.format("loom left %+.0f, loom right %+.0f", left, right));
    }

    private static float loomFromSector(CircuitPack pack, int lo, int hi) {
        LifBrain b = new LifBrain(pack, 4);
        b.warmup(3, 16);
        int[] all = pack.sensoryIndices("LPLC2");
        int per = Math.max(1, all.length / 16);
        int[] sector = new int[(hi - lo) * per];
        for (int k = 0; k < sector.length; k++) sector[k] = all[lo * per + k];

        for (int t = 0; t < 250; t++) {
            b.inject(sector, 16f * t / 250f);
            b.step();
        }
        return b.turnCommand();
    }
}
