//! The brain port against the reference: the golden spike trace from `Main trace`, and the
//! four behavioural checks from `Main.verify` with the same seeds and thresholds.

use flit::brain::{Brain, Pack, STUB};

fn brain(seed: u64) -> Brain {
    let mut b = Brain::new(Pack::parse(STUB).unwrap(), seed);
    b.warmup(3, 16);
    b
}

fn angle_deg(a: f32, b: f32) -> f64 {
    let d = (a - b) as f64;
    d.sin().atan2(d.cos()).to_degrees()
}

/// Same protocol as `Main.trace`; every spike must match the Java's, in order.
/// If this fails: compare the decay constants bit for bit first, then find the first
/// differing step (the assert names it).
#[test]
fn golden_trace_matches_java() {
    let golden: Vec<(u32, u32)> = include_str!("golden/stub_trace.txt")
        .lines()
        .map(|l| {
            let (a, b) = l.split_once(' ').unwrap();
            (a.parse().unwrap(), b.parse().unwrap())
        })
        .collect();
    let mut b = Brain::new(Pack::parse(STUB).unwrap(), 1);
    b.noise_mv = 0.0;
    b.warmup(3, 16);
    let pen_l = b.pack.by_type("PEN_a", Some("left"));
    let pen_r = b.pack.by_type("PEN_a", Some("right"));
    let lplc2 = b.pack.sensory("LPLC2").to_vec();
    let lc4 = b.pack.sensory("LC4").to_vec();
    let mut ours = vec![];
    for t in 0..3000u32 {
        if (500..1000).contains(&t) {
            b.inject(&pen_l, 9.0);
        }
        if (1200..1700).contains(&t) {
            b.inject(&pen_r, 9.0);
        }
        if (2000..2300).contains(&t) {
            let x = (t - 2000) as f32 / 300.0;
            let drive = 16.0 * x * x;
            b.inject(&lplc2, drive);
            b.inject(&lc4, drive * 0.7);
        }
        b.step();
        ours.extend(b.last_spikes().iter().map(|&s| (t, s)));
    }
    if let Some(i) = (0..ours.len().min(golden.len())).find(|&i| ours[i] != golden[i]) {
        panic!("first difference at spike {i}: rust {:?}, java {:?}", ours[i], golden[i]);
    }
    assert_eq!(ours.len(), golden.len(), "same prefix, different spike counts");
}

#[test]
fn bump_forms_and_holds() {
    let mut b = brain(1);
    let before = b.bump();
    for _ in 0..600 {
        b.step();
    }
    let after = b.bump();
    let drift = angle_deg(after.0, before.0).abs();
    println!("bump magnitude {:.3}, drift {drift:.1} deg", after.1);
    assert!(after.1 > 0.25, "no coherent bump after 600 ms: magnitude {}", after.1);
    assert!(drift < 60.0, "drift {drift:.1} deg");
}

fn pen_shift(side: &str, seed: u64) -> f64 {
    let mut b = brain(seed);
    let before = b.bump().0;
    let pen = b.pack.by_type("PEN_a", Some(side));
    for _ in 0..500 {
        b.inject(&pen, 9.0);
        b.step();
    }
    angle_deg(b.bump().0, before)
}

/// Judged over 6 seeds, as the Java: the 16-wedge attractor is lumpy, single trials stick or
/// jump two wedges.
#[test]
fn bump_rotates_with_pen_drive() {
    let (mut sum_l, mut sum_r, mut agree) = (0.0, 0.0, 0);
    for seed in 1..=6 {
        let (l, r) = (pen_shift("left", seed), pen_shift("right", seed));
        sum_l += l;
        sum_r += r;
        agree += (l.signum() != r.signum()) as u32;
    }
    let (l, r) = (sum_l / 6.0, sum_r / 6.0);
    println!("PEN left {l:+.1}, right {r:+.1} deg / 500 ms, {agree}/6 seeds");
    assert!(l > 8.0, "left PEN drive: {l:+.1} deg / 500 ms");
    assert!(r < -3.0, "right PEN drive: {r:+.1} deg / 500 ms");
    assert!(agree >= 5, "sides disagree on only {agree}/6 seeds");
}

#[test]
fn looming_triggers_escape() {
    let mut b = brain(3);
    let baseline = b.escape_run_rate();
    let lplc2 = b.pack.sensory("LPLC2").to_vec();
    let lc4 = b.pack.sensory("LC4").to_vec();
    for t in 0..300 {
        let drive = 16.0 * (t as f32 / 300.0) * (t as f32 / 300.0);
        b.inject(&lplc2, drive);
        b.inject(&lc4, drive * 0.7);
        b.step();
    }
    let (run, jump) = (b.escape_run_rate(), b.escape_jump_rate());
    println!("escape run {baseline:.0} -> {run:.0} Hz, jump {jump:.0} Hz");
    assert!(run > baseline + 20.0, "escape run {baseline:.0} -> {run:.0} Hz");
    assert!(run > jump, "DNp02 {run:.0} Hz vs DNp01 {jump:.0} Hz");
}

fn loom_from_sector(lo: usize, hi: usize) -> f32 {
    let mut b = brain(4);
    let all = b.pack.sensory("LPLC2").to_vec();
    let per = (all.len() / 16).max(1);
    let sector = &all[lo * per..hi * per];
    for t in 0..250 {
        b.inject(sector, 16.0 * t as f32 / 250.0);
        b.step();
    }
    b.turn_command()
}

#[test]
fn escape_is_directional() {
    let (left, right) = (loom_from_sector(0, 8), loom_from_sector(8, 16));
    println!("loom left {left:+.0}, loom right {right:+.0}");
    assert!(left > right, "loom left {left:+.0}, loom right {right:+.0}");
}

/// Empty readout groups read 0, not NaN (NaN would carry into the fly's position).
#[test]
fn empty_groups_read_zero() {
    let b = brain(1);
    assert_eq!(b.role_rate("forward_fast", "left"), 0.0);
    assert_eq!(b.role_rate("no_such_role", "left"), 0.0);
    assert!(b.forward_command().is_finite() && b.turn_command().is_finite());
}

/// The CSR arrays sit right after the JSON, unaligned in the stub; a truncated or corrupt file
/// is an error, not a panic.
#[test]
fn pack_loads_and_rejects_garbage() {
    let p = Pack::parse(STUB).unwrap();
    let word = |i: usize| u32::from_le_bytes(STUB[4 * i..4 * i + 4].try_into().unwrap()) as usize;
    assert_eq!(p.n, word(1));
    assert_ne!((16 + u32::from_le_bytes(STUB[12..16].try_into().unwrap())) % 4, 0, "stub no longer tests alignment");
    assert!(Pack::parse(&STUB[..STUB.len() - 1]).is_err());
    assert!(Pack::parse(b"FBP0").is_err());
    let mut bad = STUB.to_vec();
    let last = bad.len() - 4 * word(2) - 4; // last index entry -> out of range
    bad[last..last + 4].copy_from_slice(&9999u32.to_le_bytes());
    assert!(Pack::parse(&bad).is_err());
}

/// Cost of one simulated millisecond on the stub, with noise, as the app runs it. Run:
/// `cargo test --release --test brain brain_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn brain_cost() {
    let mut b = brain(1);
    let (n, t) = (200_000, std::time::Instant::now());
    let spikes: usize = (0..n).map(|_| b.step()).sum();
    let us = t.elapsed().as_secs_f64() * 1e6 / n as f64;
    println!("{us:.2} us per 1 ms step ({:.2} spikes/ms), {:.0} us per 60 Hz frame", spikes as f64 / n as f64, us * 16.7);
}
