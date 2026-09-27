//! The spiking brain: a port of `core/.../LifBrain.java` and `CircuitPack.java`.
//! Same equations, step order, constants and readouts as the Java, and the same f32 operations
//! in the same order, so a noise-free run reproduces the Java spike for spike
//! (`tests/brain.rs`, golden trace from `Main trace`).

use serde_json::Value;
use std::collections::HashMap;

/// The pack every build carries; `--pack <path>` swaps in another.
pub const STUB: &[u8] = include_bytes!("../../data/brain_stub.fbp");

/// An .fbp file, loaded (format: `extract/pack.py`).
pub struct Pack {
    pub n: usize,
    indptr: Vec<u32>,
    indices: Vec<u32>,
    weights: Vec<f32>,
    types: Vec<String>,
    sides: Vec<String>,
    /// Tonic background depolarisation in mV, per neuron.
    tonic: Vec<f32>,
    /// role -> side ("left"/"right"/"other") -> neuron indices.
    motor: HashMap<String, HashMap<String, Vec<u32>>>,
    sensory: HashMap<String, Vec<u32>>,
    /// tau_membrane, tau_synapse, v_rest, v_threshold, v_reset, refractory, dt (ms / mV).
    params: [f64; 7],
}

impl Pack {
    pub fn load(path: &std::path::Path) -> Result<Pack, String> {
        Pack::parse(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }

    /// Header, JSON metadata, then CSR arrays. The metadata has any length, so the arrays are
    /// usually not 4-byte aligned: read them with `from_le_bytes`, never by casting the slice.
    pub fn parse(b: &[u8]) -> Result<Pack, String> {
        let word = |i: usize| b.get(4 * i..4 * i + 4).map(|w| u32::from_le_bytes(w.try_into().unwrap()));
        if b.get(..4) != Some(b"FBP1") {
            return Err("not an .fbp file".into());
        }
        let [n, e, meta_len] = [1, 2, 3].map(|i| word(i).unwrap_or(0) as usize);
        let arrays = 16 + meta_len;
        if (b.len() as u64) != arrays as u64 + 4 * (n as u64 + 1) + 8 * e as u64 {
            return Err(format!("wrong size for {n} neurons, {e} edges"));
        }
        let words = |off: usize, count: usize| b[off..off + 4 * count].chunks_exact(4).map(|w| w.try_into().unwrap());
        let indptr: Vec<u32> = words(arrays, n + 1).map(u32::from_le_bytes).collect();
        let indices: Vec<u32> = words(arrays + 4 * (n + 1), e).map(u32::from_le_bytes).collect();
        let weights: Vec<f32> = words(arrays + 4 * (n + 1 + e), e).map(f32::from_le_bytes).collect();
        if indptr[n] as usize != e || indptr.windows(2).any(|w| w[0] > w[1]) || indices.iter().any(|&i| i as usize >= n) {
            return Err("corrupt CSR arrays".into());
        }

        let meta: Value = serde_json::from_slice(&b[16..arrays]).map_err(|e| format!("metadata: {e}"))?;
        let neurons = meta["neurons"].as_array().filter(|a| a.len() == n).ok_or("metadata: neurons")?;
        let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
        let types: Vec<String> = neurons.iter().map(|x| text(&x["type"])).collect();
        let sides: Vec<String> = neurons.iter().map(|x| text(&x["side"])).collect();
        let mut tonic = vec![0.0; n];
        for (ty, mv) in meta["tonic"].as_object().into_iter().flatten() {
            let mv = mv.as_f64().unwrap_or(0.0) as f32;
            types.iter().zip(&mut tonic).filter(|(t, _)| *t == ty).for_each(|(_, x)| *x = mv);
        }
        let ids = |v: &Value| -> Result<Vec<u32>, String> {
            let ids: Vec<u32> = v.as_array().into_iter().flatten().map(|x| x.as_f64().unwrap_or(-1.0) as u32).collect();
            if ids.iter().any(|&i| i as usize >= n) { Err("metadata: neuron index out of range".into()) } else { Ok(ids) }
        };
        let mut motor = HashMap::new();
        for (role, by_side) in meta["motor"].as_object().into_iter().flatten() {
            let mut m = HashMap::new();
            for (side, v) in by_side.as_object().into_iter().flatten() {
                m.insert(side.clone(), ids(v)?);
            }
            motor.insert(role.clone(), m);
        }
        let mut sensory = HashMap::new();
        for (group, v) in meta["sensory"].as_object().into_iter().flatten() {
            sensory.insert(group.clone(), ids(v)?);
        }
        let p = &meta["params"];
        let keys = ["tau_membrane_ms", "tau_synapse_ms", "v_rest_mv", "v_threshold_mv", "v_reset_mv", "refractory_ms", "dt_ms"];
        let mut params = [0.0; 7];
        for (x, k) in params.iter_mut().zip(keys) {
            *x = p[k].as_f64().ok_or(format!("metadata: params.{k}"))?;
        }
        Ok(Pack { n, indptr, indices, weights, types, sides, tonic, motor, sensory, params })
    }

    /// Neurons of a cell type, optionally on one side (matched by first letter, as the Java).
    pub fn by_type(&self, ty: &str, side: Option<&str>) -> Vec<u32> {
        let first = side.map(|s| &s[..1]);
        (0..self.n)
            .filter(|&i| self.types[i] == ty && first.is_none_or(|f| self.sides[i].starts_with(f)))
            .map(|i| i as u32)
            .collect()
    }

    pub fn motor(&self, role: &str, side: &str) -> &[u32] {
        self.motor.get(role).and_then(|m| m.get(side)).map_or(&[], |v| v)
    }

    pub fn sensory(&self, group: &str) -> &[u32] {
        self.sensory.get(group).map_or(&[], |v| v)
    }
}

/// Leaky integrate-and-fire, one millisecond per `step` (see `LifBrain.java` for the model).
pub struct Brain {
    pub pack: Pack,
    v: Vec<f32>,
    g: Vec<f32>,
    /// Injected current for the next step, mV.
    ext: Vec<f32>,
    /// Exponentially smoothed firing rate, Hz.
    rate: Vec<f32>,
    refractory: Vec<i16>,
    spikes: Vec<u32>,
    spike_count: usize,
    v_rest: f32,
    v_th: f32,
    v_reset: f32,
    decay_v: f32,
    decay_g: f32,
    rate_decay: f32,
    rate_kick: f32,
    refractory_steps: i16,
    pub noise_mv: f32,
    rng: u64,
}

impl Brain {
    pub fn new(pack: Pack, seed: u64) -> Brain {
        let [tau_m, tau_s, v_rest, v_th, v_reset, refractory_ms, dt] = pack.params;
        // Constants in f64, then cast, exactly as the Java: `(float) Math.exp(...)`.
        let rate_decay = (-dt / 50.0).exp() as f32;
        let n = pack.n;
        Brain {
            v: vec![v_rest as f32; n],
            g: vec![0.0; n],
            ext: vec![0.0; n],
            rate: vec![0.0; n],
            refractory: vec![0; n],
            spikes: vec![0; n],
            spike_count: 0,
            v_rest: v_rest as f32,
            v_th: v_th as f32,
            v_reset: v_reset as f32,
            decay_v: (-dt / tau_m).exp() as f32,
            decay_g: (-dt / tau_s).exp() as f32,
            rate_decay,
            // the Java widens the *float* rate_decay back to double here
            rate_kick: ((1.0 - rate_decay as f64) * (1000.0 / dt)) as f32,
            refractory_steps: (refractory_ms / dt + 0.5).floor() as i16, // Math.round
            noise_mv: 0.30,
            rng: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
            pack,
        }
    }

    /// Depolarise a set of neurons on the next step only.
    pub fn inject(&mut self, idx: &[u32], mv: f32) {
        for &i in idx {
            self.ext[i as usize] += mv;
        }
    }

    /// Neurons that fired on the last step.
    pub fn last_spikes(&self) -> &[u32] {
        &self.spikes[..self.spike_count]
    }

    /// Advance one millisecond. Returns the number of neurons that fired.
    pub fn step(&mut self) -> usize {
        let mut ns = 0;
        for i in 0..self.pack.n {
            if self.refractory[i] > 0 {
                self.refractory[i] -= 1;
                self.v[i] = self.v_reset;
                self.ext[i] = 0.0;
                continue;
            }
            let target = self.v_rest + self.g[i] + self.ext[i] + self.pack.tonic[i];
            let noise = self.next_noise();
            self.v[i] = target + (self.v[i] - target) * self.decay_v + noise;
            self.ext[i] = 0.0;
            if self.v[i] >= self.v_th {
                self.v[i] = self.v_reset;
                self.refractory[i] = self.refractory_steps;
                self.spikes[ns] = i as u32;
                ns += 1;
            }
        }
        for g in &mut self.g {
            *g *= self.decay_g;
        }
        // Event-driven: only the CSR rows of neurons that fired.
        let p = &self.pack;
        for &pre in &self.spikes[..ns] {
            for e in p.indptr[pre as usize] as usize..p.indptr[pre as usize + 1] as usize {
                self.g[p.indices[e] as usize] += p.weights[e];
            }
        }
        for r in &mut self.rate {
            *r *= self.rate_decay;
        }
        for &s in &self.spikes[..ns] {
            self.rate[s as usize] += self.rate_kick;
        }
        self.spike_count = ns;
        ns
    }

    /// xorshift64*, scaled to roughly uniform noise in [-noise/2, +noise/2].
    fn next_noise(&mut self) -> f32 {
        if self.noise_mv == 0.0 {
            return 0.0;
        }
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let u = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u32 << 24) as f32;
        (u - 0.5) * self.noise_mv
    }

    // -- readout -------------------------------------------------------------------------------

    /// Mean rate of a group; 0 for an empty group (BDN2 has only "other"), never NaN.
    fn mean_rate(&self, idx: &[u32]) -> f32 {
        if idx.is_empty() {
            return 0.0;
        }
        let mut t = 0.0_f32;
        for &i in idx {
            t += self.rate[i as usize];
        }
        t / idx.len() as f32
    }

    pub fn role_rate(&self, role: &str, side: &str) -> f32 {
        self.mean_rate(self.pack.motor(role, side))
    }

    /// Right minus left DNa02, plus a weighted share of the slower DNa01 pair.
    pub fn turn_command(&self) -> f32 {
        (self.role_rate("turn_fast", "right") - self.role_rate("turn_fast", "left"))
            + 0.4 * (self.role_rate("turn_sustained", "right") - self.role_rate("turn_sustained", "left"))
    }

    pub fn forward_command(&self) -> f32 {
        self.role_rate("forward_fast", "other") + 0.5 * (self.role_rate("forward", "left") + self.role_rate("forward", "right"))
    }

    pub fn escape_run_rate(&self) -> f32 {
        self.role_rate("escape_run", "left") + self.role_rate("escape_run", "right")
    }

    pub fn escape_jump_rate(&self) -> f32 {
        self.role_rate("escape_jump", "left") + self.role_rate("escape_jump", "right")
    }

    pub fn stop_rate(&self) -> f32 {
        self.role_rate("stop", "left") + self.role_rate("stop", "right")
    }

    /// Heading bump: circular mean of EPG rates, (angle radians, magnitude 0..1). A magnitude
    /// near zero means no coherent bump.
    pub fn bump(&self) -> (f32, f32) {
        let epg = self.pack.sensory("EPG");
        let (mut x, mut y, mut sum) = (0.0_f64, 0.0_f64, 0.0_f64);
        for (k, &i) in epg.iter().enumerate() {
            let theta = 2.0 * std::f64::consts::PI * k as f64 / epg.len() as f64;
            let r = self.rate[i as usize] as f64;
            x += r * theta.cos();
            y += r * theta.sin();
            sum += r;
        }
        if sum <= 0.0 {
            return (0.0, 0.0);
        }
        (y.atan2(x) as f32, (x.hypot(y) / sum) as f32)
    }

    /// Hold one wedge above threshold until recurrent excitation takes over, then let the
    /// attractor settle.
    pub fn warmup(&mut self, wedge: usize, wedge_count: usize) {
        let epg = self.pack.sensory("EPG");
        if epg.is_empty() {
            return;
        }
        let per = (epg.len() / wedge_count).max(1);
        let seed: Vec<u32> = (0..per).map(|k| epg[(wedge * per + k) % epg.len()]).collect();
        for _ in 0..200 {
            self.inject(&seed, 16.0);
            self.step();
        }
        for _ in 0..400 {
            self.step();
        }
    }
}
