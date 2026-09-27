//! Firing rates -> motion: a port of `updateBody()` in `clients/web/index.html`. The mapping is
//! a scale factor plus a first-order lag, nothing more; the one exception is the escape latch,
//! standing in for the ventral nerve cord the brain doesn't have.

use crate::fly::{CYCLE, FLY_SCALE, FlyPose, SPEED_SCALE, WALK_SPEED};
use crate::world::{self, Seg};
use flit::brain::Brain;
use std::f32::consts::PI;

/// Gait phase at distance 0 (all six feet down), as in `path.rs`.
const REST_PHASE: f64 = 0.45;
/// mV injected into each CONTACT neuron per frame's brain step, at full contact (1.0).
const CONTACT_MV: f32 = 30.0;
/// Where the fly feels from: this far ahead of the thorax, in body units (the head is at ~11,
/// the antennae reach ~20). See `world::contact`.
pub const SENSE_AHEAD: f32 = 20.0;
/// The work-area wall keeps the thorax this far inside it, in body units.
const WALL: f32 = 6.0;
/// Pressed into the wall this long without getting anywhere (a corner, or walking straight
/// into it), the fly turns away.
const STUCK_MS: f32 = 1000.0;
/// ...and keeps turning this long. A one-frame turn doesn't last: the brain holds its heading
/// (PFL3) and steers straight back into the wall.
const UNSTICK_MS: f32 = 600.0;

pub struct Body {
    /// Physical screen pixels.
    pub x: f32,
    pub y: f32,
    pub heading: f32,
    /// rad/ms and logical px/ms, lagged toward the brain's targets.
    omega: f32,
    speed: f32,
    /// ms left of the escape run after DNp02 fired.
    escape_latch: f32,
    /// Logical px walked (f64: hours of walking must not cost precision, see `path.rs`).
    walked: f64,
    /// Multiplier on the forward-command walking speed (`--walk-speed`); escape is unaffected.
    pub walk: f32,
    /// Sensing point, body units ahead of the thorax (`SENSE_AHEAD`; tests sweep it).
    pub sense_ahead: f32,
    /// Last frame's contact (left, right), 0..1, for `--debug`.
    pub contact: (f32, f32),
    /// How long the fly has been blocked by the work-area wall, and how long it has left to
    /// turn away from it.
    stuck_ms: f32,
    unstick_ms: f32,
}

impl Body {
    pub fn new((x, y): (f32, f32)) -> Body {
        Body { x, y, heading: 0.0, omega: 0.0, speed: 0.0, escape_latch: 0.0, walked: 0.0, walk: WALK_SPEED, sense_ahead: SENSE_AHEAD, contact: (0.0, 0.0), stuck_ms: 0.0, unstick_ms: 0.0 }
    }

    /// One frame: feel `segs`, hold that contact for all `steps` brain steps (injected into the
    /// CONTACT neurons like looming is), then move. Returns the spike count.
    pub fn tick(
        &mut self,
        brain: &mut Brain,
        steps: u32,
        scale: f32,
        segs: &[Seg],
        inside: impl Fn(f32, f32) -> bool,
        home: impl Fn(f32, f32) -> (f32, f32),
    ) -> usize {
        let unit = FLY_SCALE * scale;
        let a = self.sense_ahead * unit;
        let head = (self.x + self.heading.cos() * a, self.y + self.heading.sin() * a);
        self.contact = world::contact((self.x, self.y), head, self.heading, world::REACH * unit, segs);
        let left = brain.pack.sensory("CONTACT_left").to_vec();
        let right = brain.pack.sensory("CONTACT_right").to_vec();
        let mut spikes = 0;
        for _ in 0..steps {
            brain.inject(&left, CONTACT_MV * self.contact.0);
            brain.inject(&right, CONTACT_MV * self.contact.1);
            spikes += brain.step();
        }
        self.update(brain, steps as f32, scale, inside, home);
        spikes
    }

    /// Advances `dt_ms` of brain time. `inside(x, y)`: is this screen point on some monitor's
    /// work area; `home(x, y)`: centre of the work area nearest to it.
    pub fn update(
        &mut self,
        brain: &mut Brain,
        dt_ms: f32,
        scale: f32,
        inside: impl Fn(f32, f32) -> bool,
        home: impl Fn(f32, f32) -> (f32, f32),
    ) {
        let turn = brain.turn_command();
        let fwd = brain.forward_command();
        let escape = brain.escape_run_rate();
        let jump = brain.escape_jump_rate();
        let stop = brain.stop_rate();

        if escape > 35.0 {
            self.escape_latch = 420.0;
        } else {
            self.escape_latch = (self.escape_latch - dt_ms).max(0.0);
        }
        let running = self.escape_latch > 0.0;
        let frozen = stop > 95.0 && !running;
        let startled = !frozen && jump > 25.0;

        let target_omega = turn * 0.00013;
        let mut target_speed = fwd * 0.00085 * self.walk + escape * 0.0042 + if running { 0.22 * (self.escape_latch / 420.0) } else { 0.0 };
        if frozen {
            target_speed = 0.0;
        }
        if startled {
            target_speed += 0.52; // ponytail: the web client's wing buzz isn't drawn; add with wing animation
        }
        target_speed *= SPEED_SCALE; // the web client's gains are for the FLY_SCALE = 1.9 fly

        // First-order lag stands in for leg and body inertia.
        let k = 1.0 - (-dt_ms / 55.0).exp();
        self.omega += (target_omega - self.omega) * k;
        self.speed += (target_speed - self.speed) * k;

        self.heading = (self.heading + self.omega * dt_ms + PI).rem_euclid(2.0 * PI) - PI;
        let step = self.speed * dt_ms;
        self.walked += step as f64;
        let (old_x, old_y) = (self.x, self.y);
        self.x += self.heading.cos() * step * scale;
        self.y += self.heading.sin() * step * scale;

        // The work-area border is a hard wall (window edges are sensed and crossable; following
        // any edge is the brain's job, see `tick`). Blocked, the fly slides along the wall, per
        // axis. Still getting nowhere after STUCK_MS (a corner, or walking straight into it),
        // it turns toward the middle of its monitor and feeds that turn to the PENs, so the
        // brain's heading estimate agrees with the body.
        if !inside(self.x, self.y) {
            (self.x, self.y) = home(self.x, self.y); // monitor unplugged, taskbar moved...
        }
        let m = WALL * FLY_SCALE * scale;
        let mut blocked = false;
        if !inside(self.x - m, self.y) || !inside(self.x + m, self.y) {
            self.x = old_x;
            blocked = true;
        }
        if !inside(self.x, self.y - m) || !inside(self.x, self.y + m) {
            self.y = old_y;
            blocked = true;
        }
        let moved = (self.x - old_x).hypot(self.y - old_y);
        let stuck = blocked && moved < 0.3 * step * scale;
        self.stuck_ms = if stuck { self.stuck_ms + dt_ms } else { 0.0 };
        if self.stuck_ms > STUCK_MS {
            self.unstick_ms = UNSTICK_MS;
        }
        self.unstick_ms = (self.unstick_ms - dt_ms).max(0.0);
        if self.unstick_ms > 0.0 {
            let (hx, hy) = home(self.x, self.y);
            let inward = (hy - self.y).atan2(hx - self.x);
            let diff = (inward - self.heading).sin().atan2((inward - self.heading).cos());
            self.heading += diff * 0.16;
            let pen = brain.pack.by_type("PEN_a", Some(if diff > 0.0 { "left" } else { "right" }));
            brain.inject(&pen, 5.5);
        }
    }

    pub fn pose(&self, scale: f32) -> FlyPose {
        FlyPose {
            x: self.x,
            y: self.y,
            heading: self.heading,
            speed: self.speed * 1000.0 * scale,
            gait_phase: (REST_PHASE + self.walked / (FLY_SCALE * CYCLE) as f64).rem_euclid(1.0) as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flit::brain::{Pack, STUB};

    /// Runs the brain-driven body for `frames` 60 Hz frames at 100% (16 brain steps each),
    /// calling `stim(brain, ms)` before every step. Returns each frame's speed in logical px/s.
    fn run(brain: &mut Brain, body: &mut Body, frames: u32, mut stim: impl FnMut(&mut Brain, u32)) -> Vec<f32> {
        let mut ms = 0;
        (0..frames)
            .map(|_| {
                for _ in 0..16 {
                    stim(brain, ms);
                    brain.step();
                    ms += 1;
                }
                body.update(brain, 16.0, 1.0, |_, _| true, |x, y| (x, y));
                body.speed * 1000.0
            })
            .collect()
    }

    /// Legs must step slowly enough to see at 60 fps: step frequency = speed / px per gait
    /// cycle, at the brain's typical forward command. Escape is printed, not asserted (a
    /// fleeing fly's legs may blur).
    #[test]
    fn steps_slow_enough_to_see() {
        let cycle_px = CYCLE * FLY_SCALE;
        let mut brain = Brain::new(Pack::parse(STUB).unwrap(), 1);
        brain.warmup(3, 16);
        let mut body = Body::new((0.0, 0.0));
        run(&mut brain, &mut body, 120, |_, _| {}); // settle
        let walk = run(&mut brain, &mut body, 300, |_, _| {});
        let walking = walk.iter().sum::<f32>() / walk.len() as f32;

        let (lplc2, lc4) = (brain.pack.sensory("LPLC2").to_vec(), brain.pack.sensory("LC4").to_vec());
        let escape = run(&mut brain, &mut body, 60, |b, ms| {
            if ms < 300 {
                let drive = 16.0 * (ms as f32 / 300.0).powi(2);
                b.inject(&lplc2, drive);
                b.inject(&lc4, drive * 0.7);
            }
        });
        let fleeing = escape.iter().cloned().fold(0.0, f32::max);
        let hz = walking / cycle_px;
        println!(
            "walking {walking:.0} px/s -> {hz:.1} steps/s; escape peak {fleeing:.0} px/s -> {:.1} steps/s ({cycle_px:.2} px per cycle)",
            fleeing / cycle_px
        );
        assert!(hz <= 8.0, "legs step {hz:.1} times a second while walking");
    }

    // --- Edge following, closed loop: brain + body + fixed segments, at 100% -----------------

    use crate::world::{BODY_LENGTH, REACH, Rect, border};

    /// One body length in px at 100%.
    const BL: f32 = BODY_LENGTH * FLY_SCALE;
    /// Turning faster than this is a spin, not steering: the trial fails.
    const SPIN_DEG_S: f32 = 400.0;

    /// Runs a fly from `(x, y, heading)` among `segs`, walled in by `work`, 60 Hz frames of 16
    /// brain steps, until `each(body)` returns Some(verdict) or `secs` run out (false). A spin
    /// fails it. Returns (verdict, fastest turn in deg/s).
    fn trial(t: &Setup, seed: u64, start: (f32, f32, f32), segs: &[Seg], work: Rect, secs: f32, mut each: impl FnMut(&Body) -> Option<bool>) -> (bool, f32) {
        let pack = t.pack;
        let mut brain = Brain::new(Pack::parse(pack).unwrap(), seed);
        brain.warmup(3, 16);
        let mut body = Body::new((start.0, start.1));
        body.heading = start.2;
        body.sense_ahead = t.ahead;
        let inside = |x: f32, y: f32| x >= work.l && x < work.r && y >= work.t && y < work.b;
        let home = |_: f32, _: f32| ((work.l + work.r) / 2.0, (work.t + work.b) / 2.0);
        let mut fastest = 0.0_f32;
        for _ in 0..(secs * 60.0) as u32 {
            body.tick(&mut brain, 16, 1.0, segs, inside, home);
            fastest = fastest.max(body.omega.abs() * 1000.0 * 180.0 / PI);
            if fastest > SPIN_DEG_S {
                return (false, fastest);
            }
            if let Some(v) = each(&body) {
                return (v, fastest);
            }
        }
        (false, fastest)
    }

    /// What a scenario runs with: the pack, and the sensing distance.
    struct Setup<'a> {
        pack: &'a [u8],
        ahead: f32,
    }

    const FAR: Rect = Rect { l: -1e5, t: -1e5, r: 1e5, b: 1e5 };
    /// A window edge along y = 0, crossable.
    const EDGE: [Seg; 1] = [Seg { a: (-1e5, 0.0), b: (1e5, 0.0) }];

    /// Meets a window edge at a shallow angle, starting where it first feels it. Once it first
    /// reaches the edge (within 0.3 BL), it must cover 8 BL staying within 0.6 BL of the line,
    /// either side: weaving across counts.
    fn follows_edge(t: &Setup, seed: u64, deg: f32) -> (bool, f32) {
        let y0 = REACH * FLY_SCALE;
        let (mut reached, mut path, mut prev) = (false, 0.0, (0.0_f32, y0));
        trial(t, seed, (0.0, y0, -deg.to_radians()), &EDGE, FAR, 40.0, |b| {
            let step = (b.x - prev.0).hypot(b.y - prev.1);
            prev = (b.x, b.y);
            reached |= b.y.abs() < 0.3 * BL;
            if !reached {
                return None;
            }
            if b.y.abs() > 0.6 * BL {
                return Some(false);
            }
            path += step;
            (path >= 8.0 * BL).then_some(true)
        })
    }

    /// Meets it nearly head-on: must cross and carry on 2 BL beyond it.
    fn crosses_edge(t: &Setup, seed: u64) -> (bool, f32) {
        let y0 = REACH * FLY_SCALE;
        trial(t, seed, (0.0, y0, -85f32.to_radians()), &EDGE, FAR, 10.0, |b| (b.y < -2.0 * BL).then_some(true))
    }

    /// Walks parallel to the bottom screen border, 10 px inside: follows it for 8 BL.
    fn follows_border(t: &Setup, seed: u64) -> (bool, f32) {
        let work = Rect { l: -1e5, t: -1e5, r: 1e5, b: 0.0 };
        let (mut path, mut prev) = (0.0, (0.0_f32, -10.0_f32));
        trial(t, seed, (0.0, -10.0, 0.0), &border(&[work]), work, 40.0, |b| {
            path += (b.x - prev.0).hypot(b.y - prev.1);
            prev = (b.x, b.y);
            if b.y < -1.2 * BL {
                return Some(false);
            }
            (path >= 8.0 * BL).then_some(true)
        })
    }

    /// Starts in the bottom-left corner of a screen, facing into it: must get 2 BL away from
    /// the corner within 3 s.
    fn leaves_corner(t: &Setup, seed: u64) -> (bool, f32) {
        let work = Rect { l: 0.0, t: 0.0, r: 1200.0, b: 800.0 };
        let start = (10.0, 790.0, 135f32.to_radians());
        trial(t, seed, start, &border(&[work]), work, 3.0, |b| (b.x.hypot(800.0 - b.y) > 2.0 * BL).then_some(true))
    }

    /// Passes out of `seeds` for: shallow 20 deg, shallow 30 deg, steep, border, corner; and the
    /// fastest turn in any passing trial, deg/s.
    fn score(t: &Setup, seeds: u64) -> ([u32; 5], f32) {
        let mut fastest = 0.0_f32;
        let mut count = |f: &dyn Fn(u64) -> (bool, f32)| {
            (1..=seeds)
                .filter(|&s| {
                    let (ok, turn) = f(s);
                    if ok {
                        fastest = fastest.max(turn);
                    }
                    ok
                })
                .count() as u32
        };
        let s = [
            count(&|s| follows_edge(t, s, 20.0)),
            count(&|s| follows_edge(t, s, 30.0)),
            count(&|s| crosses_edge(t, s)),
            count(&|s| follows_border(t, s)),
            count(&|s| leaves_corner(t, s)),
        ];
        (s, fastest)
    }

    /// The acceptance bar on the shipped stub pack: every scenario in at least 10 of 12 seeds,
    /// no spins.
    #[test]
    fn edge_following() {
        let (s, fastest) = score(&Setup { pack: STUB, ahead: SENSE_AHEAD }, 12);
        println!("shallow 20 {}/12, shallow 30 {}/12, steep {}/12, border {}/12, corner {}/12, fastest turn {fastest:.0} deg/s", s[0], s[1], s[2], s[3], s[4]);
        assert!(s.iter().all(|&n| n >= 10), "edge following: {s:?}");
    }

    /// The CONTACT weight sweep, 6 seeds (12 with `FLIT_SEEDS=12`). Packs come from
    /// `extract/make_stub_pack.py` with `w_contact_dn` overridden, named `w<weight>.fbp`, in the
    /// directory `FLIT_SWEEP`. Run:
    /// `FLIT_SWEEP=<dir> cargo test --release sweep_contact -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn sweep_contact() {
        let dir = std::env::var("FLIT_SWEEP").expect("FLIT_SWEEP=<dir of w*.fbp>");
        let seeds: u64 = std::env::var("FLIT_SEEDS").map_or(6, |s| s.parse().unwrap());
        let mut packs: Vec<(f32, std::path::PathBuf)> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| {
                let p = e.ok()?.path();
                Some((p.file_stem()?.to_str()?.strip_prefix('w')?.parse().ok()?, p))
            })
            .collect();
        packs.sort_by(|a, b| a.0.total_cmp(&b.0));
        println!("| w_contact_dn | shallow 20 | shallow 30 | steep | border | corner | fastest turn deg/s |");
        println!("|---|---|---|---|---|---|---|");
        for (w, p) in packs {
            let (s, fastest) = score(&Setup { pack: &std::fs::read(p).unwrap(), ahead: SENSE_AHEAD }, seeds);
            let n = |i: usize| format!("{}/{seeds}", s[i]);
            println!("| {w} | {} | {} | {} | {} | {} | {fastest:.0} |", n(0), n(1), n(2), n(3), n(4));
        }
    }
}
