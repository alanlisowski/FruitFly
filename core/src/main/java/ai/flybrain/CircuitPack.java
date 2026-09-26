package ai.flybrain;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * An .fbp file, loaded.
 *
 * <p>The format is deliberately boring: a header, a JSON metadata blob, then
 * three flat arrays forming a CSR adjacency keyed by presynaptic neuron. That
 * layout is chosen for how the simulation reads it -- each millisecond we have
 * a short list of neurons that fired, and for each one we want its outgoing
 * edges contiguous in memory. One CSR row, one cache-friendly sweep.
 *
 * <p>No graph library, no object per synapse. At 300k edges the difference
 * between this and an object graph is the difference between fitting in L2 and
 * not.
 */
public final class CircuitPack {

    private static final int MAGIC = 0x31504246; // "FBP1" little-endian

    public final int neuronCount;
    public final int edgeCount;

    /** CSR row offsets, length neuronCount + 1. */
    public final int[] indptr;
    /** Postsynaptic neuron index per edge. */
    public final int[] indices;
    /** Signed millivolts delivered to the target per presynaptic spike. */
    public final float[] weights;

    public final String[] type;
    public final String[] side;
    public final String[] block;

    /** Tonic background depolarisation in mV, per neuron. */
    public final float[] tonic;

    /** role -> side ("left"/"right"/"other") -> neuron indices. */
    public final Map<String, Map<String, int[]>> motor;
    /** sensory group name -> neuron indices. */
    public final Map<String, int[]> sensory;

    public final Params params;

    public record Params(double tauMembraneMs, double tauSynapseMs, double vRestMv,
                         double vThresholdMv, double vResetMv, double refractoryMs,
                         double dtMs) {}

    private CircuitPack(int n, int e, int[] indptr, int[] indices, float[] weights,
                        String[] type, String[] side, String[] block, float[] tonic,
                        Map<String, Map<String, int[]>> motor, Map<String, int[]> sensory,
                        Params params) {
        this.neuronCount = n; this.edgeCount = e;
        this.indptr = indptr; this.indices = indices; this.weights = weights;
        this.type = type; this.side = side; this.block = block; this.tonic = tonic;
        this.motor = motor; this.sensory = sensory; this.params = params;
    }

    public static CircuitPack load(Path path) throws IOException {
        ByteBuffer buf = ByteBuffer.wrap(Files.readAllBytes(path)).order(ByteOrder.LITTLE_ENDIAN);

        if (buf.getInt() != MAGIC) {
            throw new IOException(path + " is not an .fbp file");
        }
        int n = buf.getInt();
        int e = buf.getInt();
        int metaLen = buf.getInt();

        byte[] metaBytes = new byte[metaLen];
        buf.get(metaBytes);
        Json.Obj meta = Json.parse(new String(metaBytes, java.nio.charset.StandardCharsets.UTF_8)).asObj();

        int[] indptr = new int[n + 1];
        for (int i = 0; i <= n; i++) indptr[i] = buf.getInt();
        int[] indices = new int[e];
        for (int i = 0; i < e; i++) indices[i] = buf.getInt();
        float[] weights = new float[e];
        for (int i = 0; i < e; i++) weights[i] = buf.getFloat();

        if (indptr[n] != e) {
            throw new IOException("corrupt pack: indptr ends at " + indptr[n] + ", expected " + e);
        }

        List<Json.Value> neurons = meta.get("neurons").asArray();
        if (neurons.size() != n) {
            throw new IOException("header says " + n + " neurons, metadata has " + neurons.size());
        }
        String[] type = new String[n], side = new String[n], block = new String[n];
        for (int i = 0; i < n; i++) {
            Json.Obj nr = neurons.get(i).asObj();
            type[i] = nr.str("type");
            side[i] = nr.str("side");
            block[i] = nr.str("block");
        }

        float[] tonic = new float[n];
        Json.Value tv = meta.get("tonic");
        if (tv != null) {
            for (Map.Entry<String, Json.Value> en : tv.asObj().entries()) {
                float mv = (float) en.getValue().asDouble();
                for (int i = 0; i < n; i++) if (type[i].equals(en.getKey())) tonic[i] = mv;
            }
        }

        Map<String, Map<String, int[]>> motor = new HashMap<>();
        Json.Value mv = meta.get("motor");
        if (mv != null) {
            for (Map.Entry<String, Json.Value> role : mv.asObj().entries()) {
                Map<String, int[]> bySide = new HashMap<>();
                for (Map.Entry<String, Json.Value> sd : role.getValue().asObj().entries()) {
                    bySide.put(sd.getKey(), toIntArray(sd.getValue()));
                }
                motor.put(role.getKey(), bySide);
            }
        }

        Map<String, int[]> sensory = new HashMap<>();
        Json.Value sv = meta.get("sensory");
        if (sv != null) {
            for (Map.Entry<String, Json.Value> g : sv.asObj().entries()) {
                sensory.put(g.getKey(), toIntArray(g.getValue()));
            }
        }

        Json.Obj p = meta.get("params").asObj();
        Params params = new Params(
                p.num("tau_membrane_ms"), p.num("tau_synapse_ms"), p.num("v_rest_mv"),
                p.num("v_threshold_mv"), p.num("v_reset_mv"), p.num("refractory_ms"),
                p.num("dt_ms"));

        return new CircuitPack(n, e, indptr, indices, weights, type, side, block,
                tonic, motor, sensory, params);
    }

    private static int[] toIntArray(Json.Value v) {
        List<Json.Value> list = v.asArray();
        int[] out = new int[list.size()];
        for (int i = 0; i < out.length; i++) out[i] = (int) list.get(i).asDouble();
        return out;
    }

    /** Neuron indices whose cell type matches, optionally restricted to one side. */
    public int[] byType(String cellType, String sideFilter) {
        List<Integer> hits = new ArrayList<>();
        for (int i = 0; i < neuronCount; i++) {
            if (!type[i].equals(cellType)) continue;
            if (sideFilter != null && !side[i].startsWith(sideFilter.substring(0, 1))) continue;
            hits.add(i);
        }
        int[] out = new int[hits.size()];
        for (int i = 0; i < out.length; i++) out[i] = hits.get(i);
        return out;
    }

    public int[] motorIndices(String role, String sideKey) {
        Map<String, int[]> m = motor.get(role);
        if (m == null) return new int[0];
        int[] idx = m.get(sideKey);
        return idx == null ? new int[0] : idx;
    }

    public int[] sensoryIndices(String group) {
        return sensory.getOrDefault(group, new int[0]);
    }
}
