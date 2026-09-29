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
/// ...and keeps turning this long. A one-frame turn doesn't last: contact with the walls pulls
/// the fly straight back in.
const UNSTICK_MS: f32 = 600.0;
/// Sliding along the wall while pressing into it for a random 1.5-4 s (so it doesn't look
/// mechanical), the fly changes course away from it by a random 90-150 deg...
const PRESS_MS: (f32, f32) = (1500.0, 4000.0);
const COURSE_TURN_DEG: (f32, f32) = (90.0, 150.0);
/// The press clock runs at full speed with the fly heading this far or more into the wall, and
/// proportionally slower below: skimming along the border (edge following) lasts a while,
/// pushing into it doesn't, and neither lasts forever. (A hard angle cutoff can't tell them
/// apart: the live bug was a fly skimming at 10-20 deg for minutes.)
const PRESS_FULL_DEG: f32 = 30.0;
/// ...turning at this rate, with the PENs of the turning side driven throughout, so the heading
/// bump turns with the body.
const COURSE_RATE: f32 = 240.0 / 180.0 * PI / 1000.0; // rad/ms
const PEN_MV: f32 = 5.5;

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
    /// Last frame's contact (left, right), 0..1, after adaptation, for `--debug`.
    pub contact: (f32, f32),
    /// Last frame's visual contact (left, right), before max-ing with geometry, for `--debug`.
    pub vis: (f32, f32),
    pub adapt: world::Adapt,
    /// How long the fly has been pressing into the wall while sliding along it, the (random)
    /// limit, and what's left of a course change, in radians (signed).
    press_ms: f32,
    press_limit: f32,
    course_turn: f32,
    /// PEN drive (left, right), mV, held for the next frame's brain steps.
    pen: (f32, f32),
    rng: u64,
    /// How long the fly has been blocked by the work-area wall, and how long it has left to
    /// turn away from it.
    stuck_ms: f32,
    unstick_ms: f32,
}

impl Body {
    pub fn new((x, y): (f32, f32)) -> Body {
        Body { x, y, heading: 0.0, omega: 0.0, speed: 0.0, escape_latch: 0.0, walked: 0.0, walk: WALK_SPEED, sense_ahead: SENSE_AHEAD, contact: (0.0, 0.0), vis: (0.0, 0.0), adapt: world::Adapt::new(), press_ms: 0.0, press_limit: 2500.0, course_turn: 0.0, pen: (0.0, 0.0), rng: 0x9E37_79B9_7F4A_7C15, stuck_ms: 0.0, unstick_ms: 0.0 }
    }

    /// xorshift64*: uniform in [a, b).
    fn uniform(&mut self, (a, b): (f32, f32)) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let u = (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u32 << 24) as f32;
        a + (b - a) * u
    }

    /// The sensing point, screen px.
    pub fn head(&self, scale: f32) -> (f32, f32) {
        let a = self.sense_ahead * FLY_SCALE * scale;
        (self.x + self.heading.cos() * a, self.y + self.heading.sin() * a)
    }

    /// One frame: feel `segs` and see `seen` (edge points from `vision`; per side the stronger of
    /// the two, through adaptation), hold that contact for all `steps` brain steps
    /// (injected into the CONTACT neurons like looming is), then move. Returns the spike count.
    pub fn tick(
        &mut self,
        brain: &mut Brain,
        steps: u32,
        scale: f32,
        segs: &[Seg],
        seen: &[(f32, f32)],
        inside: impl Fn(f32, f32) -> bool,
        home: impl Fn(f32, f32) -> (f32, f32),
    ) -> usize {
        let (head, reach) = (self.head(scale), world::REACH * FLY_SCALE * scale);
        let geo = world::contact((self.x, self.y), head, self.heading, reach, segs);
        self.vis = world::seen((self.x, self.y), head, self.heading, reach, seen);
        let raw = (geo.0.max(self.vis.0), geo.1.max(self.vis.1));
        self.contact = self.adapt.step(raw, steps as f32);
        let left = brain.pack.sensory("CONTACT_left").to_vec();
        let right = brain.pack.sensory("CONTACT_right").to_vec();
        let (pen_l, pen_r) = (brain.pack.by_type("PEN_a", Some("left")), brain.pack.by_type("PEN_a", Some("right")));
        let mut spikes = 0;
        for _ in 0..steps {
            brain.inject(&left, CONTACT_MV * self.contact.0);
            brain.inject(&right, CONTACT_MV * self.contact.1);
            brain.inject(&pen_l, self.pen.0);
            brain.inject(&pen_r, self.pen.1);
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
        // axis. Two ways out, both turning the body AND driving the PENs of that side, so the
        // heading bump turns with it:
        // - pressing into the wall while sliding along it for PRESS_MS: a course change away
        //   from the wall by COURSE_TURN_DEG (otherwise it slides along the border forever);
        // - getting nowhere for STUCK_MS (a corner, or walking straight in): turn toward the
        //   middle of its monitor for UNSTICK_MS.
        if !inside(self.x, self.y) {
            (self.x, self.y) = home(self.x, self.y); // monitor unplugged, taskbar moved...
        }
        let m = WALL * FLY_SCALE * scale;
        let (mut blocked, mut inward) = (false, (0.0_f32, 0.0_f32));
        let (left_out, right_out) = (!inside(self.x - m, self.y), !inside(self.x + m, self.y));
        if left_out || right_out {
            self.x = old_x;
            blocked = true;
            inward.0 = left_out as i32 as f32 - right_out as i32 as f32;
        }
        let (top_out, bottom_out) = (!inside(self.x, self.y - m), !inside(self.x, self.y + m));
        if top_out || bottom_out {
            self.y = old_y;
            blocked = true;
            inward.1 = top_out as i32 as f32 - bottom_out as i32 as f32;
        }
        self.pen = (0.0, 0.0);
        let wrap = |a: f32| a.sin().atan2(a.cos());
        let into_wall = -(self.heading.cos() * inward.0 + self.heading.sin() * inward.1) / inward.0.hypot(inward.1).max(1e-6);
        let pressing = blocked && into_wall > 0.0 && self.course_turn == 0.0;
        let rate = (into_wall / PRESS_FULL_DEG.to_radians().sin()).min(1.0);
        self.press_ms = if pressing { self.press_ms + dt_ms * rate } else { 0.0 };
        if self.press_ms > self.press_limit {
            let away = wrap(inward.1.atan2(inward.0) - self.heading);
            let amount = self.uniform(COURSE_TURN_DEG).to_radians();
            self.course_turn = if away < 0.0 { -amount } else { amount };
            self.press_limit = self.uniform(PRESS_MS);
            self.press_ms = 0.0;
        }
        if self.course_turn != 0.0 {
            let d = self.course_turn.signum() * (COURSE_RATE * dt_ms).min(self.course_turn.abs());
            self.heading = wrap(self.heading + d);
            self.course_turn -= d;
            // Heading increasing = turning right on screen; left PEN drive walks the bump that way
            // (same convention as milestone 3's bounce).
            self.pen = if d > 0.0 { (PEN_MV, 0.0) } else { (0.0, PEN_MV) };
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
            self.pen = if diff > 0.0 { (PEN_MV, 0.0) } else { (0.0, PEN_MV) };
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
            body.tick(&mut brain, 16, 1.0, segs, &[], inside, home);
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

    /// Walking down into the bottom border at 70 deg: it must slide along it, change course,
    /// and get more than 1 BL away within 20 s. (The live bug: the fly slid along the bottom
    /// border forever, heading into the taskbar.) Usually ~4 s; longer when far contact pulls it
    /// back after the course change and it takes adaptation to let go.
    #[test]
    fn leaves_the_border_it_presses_into() {
        let work = Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1020.0 };
        for seed in 1..=6 {
            let t = Setup { pack: STUB, ahead: SENSE_AHEAD };
            let mut secs = 0.0;
            let (ok, _) = trial(&t, seed, (600.0, 1005.0, 70f32.to_radians()), &border(&[work]), work, 20.0, |b| {
                secs += 1.0 / 60.0;
                (1020.0 - b.y > BL).then_some(true)
            });
            println!("seed {seed}: left the border after {secs:.1} s");
            assert!(ok, "seed {seed}: still on the bottom border after 20 s");
        }
    }

    /// Five minutes on a 1920 x 1020 desktop with three overlapping windows: the fly must not
    /// live on the border (< 35% of the time within 1 BL of it), must visit edges of at least 2
    /// of the 3 windows (within 1 BL), and no single edge may hold it for more than 40 s.
    #[test]
    fn soak_five_minutes() {
        use crate::world::{distance, feelable, visible_edges_by_window};
        let work = [Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1020.0 }];
        let windows = [
            Rect { l: 250.0, t: 150.0, r: 950.0, b: 650.0 },
            Rect { l: 700.0, t: 400.0, r: 1500.0, b: 900.0 },
            Rect { l: 1200.0, t: 120.0, r: 1750.0, b: 600.0 },
        ];
        let segs = feelable(&windows, &work);
        let per_window = visible_edges_by_window(&windows);
        let edge_of = |p: (f32, f32)| {
            // the one edge within 1 BL, if any: (index into segs)
            segs.iter()
                .enumerate()
                .map(|(i, s)| (i, distance(p, std::slice::from_ref(s))))
                .filter(|&(_, d)| d < BL)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let border_segs = border(&work);
        let mut failures = vec![];
        for seed in 1..=6u64 {
            let mut brain = Brain::new(Pack::parse(STUB).unwrap(), seed);
            brain.warmup(3, 16);
            let mut body = Body::new((960.0, 510.0));
            let inside = |x: f32, y: f32| x >= 0.0 && x < 1920.0 && y >= 0.0 && y < 1020.0;
            let (mut near_border, mut visited, mut run, mut longest) = (0u32, [false; 3], (None, 0u32), 0u32);
            let frames = 60 * 300;
            for _ in 0..frames {
                body.tick(&mut brain, 16, 1.0, &segs, &[], inside, |_, _| (960.0, 510.0));
                let p = (body.x, body.y);
                near_border += (distance(p, &border_segs) < BL) as u32;
                for (w, v) in visited.iter_mut().enumerate() {
                    *v |= distance(p, &per_window[w]) < BL;
                }
                let e = edge_of(p);
                run = if e.is_some() && e == run.0 { (e, run.1 + 1) } else { (e, 1) };
                if e.is_some() {
                    longest = longest.max(run.1);
                }
            }
            let (border_pct, windows_hit, longest_s) = (near_border as f32 / frames as f32 * 100.0, visited.iter().filter(|&&v| v).count(), longest as f32 / 60.0);
            println!("seed {seed}: near border {border_pct:.0}% of the time, windows visited {windows_hit}/3, longest on one edge {longest_s:.1} s");
            if border_pct >= 35.0 || windows_hit < 2 || longest_s > 40.0 {
                failures.push(seed);
            }
        }
        assert!(failures.is_empty(), "soak failed for seeds {failures:?}");
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
