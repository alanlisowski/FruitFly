//! Firing rates -> motion: a port of `updateBody()` in `clients/web/index.html`. The mapping is
//! a scale factor plus a first-order lag, nothing more; the one exception is the escape latch,
//! standing in for the ventral nerve cord the brain doesn't have.

use crate::fly::{CYCLE, FLY_SCALE, FlyPose, SPEED_SCALE, WALK_SPEED};
use flit::brain::Brain;
use std::f32::consts::PI;

/// Gait phase at distance 0 (all six feet down), as in `path.rs`.
const REST_PHASE: f64 = 0.45;

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
}

impl Body {
    pub fn new((x, y): (f32, f32)) -> Body {
        Body { x, y, heading: 0.0, omega: 0.0, speed: 0.0, escape_latch: 0.0, walked: 0.0, walk: WALK_SPEED }
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

        // TEMPORARY (milestone 4): keep a margin of work area around the fly on every side, per
        // axis (so it slides along an edge), turn it toward the middle of its monitor, and feed
        // that turn to the PENs so the brain's heading estimate agrees with the body. Real
        // edge-following through the circuit replaces this.
        if !inside(self.x, self.y) {
            (self.x, self.y) = home(self.x, self.y); // monitor unplugged, taskbar moved...
        }
        let m = 30.0 * FLY_SCALE * scale;
        let mut bounced = false;
        if !inside(self.x - m, self.y) || !inside(self.x + m, self.y) {
            self.x = old_x;
            bounced = true;
        }
        if !inside(self.x, self.y - m) || !inside(self.x, self.y + m) {
            self.y = old_y;
            bounced = true;
        }
        if bounced {
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
        assert!(hz <= 11.0, "legs step {hz:.1} times a second while walking");
    }
}
