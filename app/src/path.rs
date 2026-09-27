//! The hardcoded route (milestone 2 only; the brain replaces this in milestone 3).
//!
//! A figure-eight with varying speed and one full stop per lap. Everything here is in
//! *logical* pixels (96 dpi); `main` scales to physical pixels, so a DPI change never
//! changes where the fly is along its route or how fast it walks.

use crate::fly::{CYCLE, FLY_SCALE, SPEED_SCALE};
use std::f64::consts::TAU;

// Figure-eight (lemniscate of Gerono): x = A sin t, y = (B/2) sin 2t. 600 wide, 100 tall each side.
const A: f64 = 300.0;
const B: f64 = 200.0;

/// Logical px the body walks per gait cycle (see `fly::CYCLE`).
const CYCLE_PX: f64 = (FLY_SCALE * CYCLE) as f64;
/// Gait phase at distance 0: the middle of the window where all six feet are on the ground.
const REST_PHASE: f64 = 0.45;

const SPEED: f64 = SPEED_SCALE as f64;
const ACCEL: f64 = 300.0 * SPEED; // px/s^2, used for speeding up from and braking to a stop
const START_SPEED: f64 = 12.0 * SPEED; // px/s: the fly never starts from exactly 0 (sqrt(0) would stall)
const DWELL: f32 = 1.8; // seconds standing still, once per lap

/// One frame's worth of output, still in logical units relative to the route's centre.
pub struct Step {
    pub x: f32,
    pub y: f32,
    pub heading: f32,
    /// Logical px per second.
    pub speed: f32,
    pub gait_phase: f32,
}

pub struct Walker {
    /// Total distance walked. f64: after hours of walking f32 would lose sub-pixel precision
    /// and the legs would start to shimmer.
    d: f64,
    /// Distance where the current run started / must end (a stop).
    leave: f64,
    stop: f64,
    dwell: f32,
    speed: f32,
    /// `cum[i]` = arc length of the loop from t = 0 to t = i * TAU / N. Lets us turn "distance
    /// walked" into the curve parameter t (a figure-eight isn't parametrised by distance).
    cum: Vec<f64>,
}

const N: usize = 4096;

impl Walker {
    pub fn new() -> Self {
        let pt = |i: usize| {
            let t = i as f64 * TAU / N as f64;
            (A * t.sin(), B / 2.0 * (2.0 * t).sin())
        };
        let mut cum = vec![0.0];
        for i in 1..=N {
            let (a, b) = (pt(i - 1), pt(i));
            cum.push(cum[i - 1] + (b.0 - a.0).hypot(b.1 - a.1));
        }
        let mut w = Walker { d: 0.0, leave: 0.0, stop: 0.0, dwell: 0.0, speed: 0.0, cum };
        w.stop = w.next_stop(0.0);
        w
    }

    fn lap(&self) -> f64 {
        self.cum[N]
    }

    /// The next place to stop, about one lap after `from`. Stops are only allowed at multiples
    /// of half a gait cycle: that is exactly where the gait phase is in a window where all six
    /// feet are planted, so the fly can come to rest without a foot left hanging in the air.
    fn next_stop(&self, from: f64) -> f64 {
        let half = CYCLE_PX / 2.0;
        ((from + self.lap()) / half).round() * half
    }

    /// Curve parameter t for a distance walked (wraps every lap).
    fn t_at(&self, d: f64) -> f64 {
        let s = d.rem_euclid(self.lap());
        let i = (self.cum.partition_point(|&c| c <= s) - 1).min(N - 1);
        let f = (s - self.cum[i]) / (self.cum[i + 1] - self.cum[i]);
        (i as f64 + f) * TAU / N as f64
    }

    /// Advance by `dt` seconds of real time.
    pub fn step(&mut self, dt: f32) -> Step {
        if self.dwell > 0.0 {
            self.dwell -= dt;
            if self.dwell <= 0.0 {
                self.leave = self.d;
                self.stop = self.next_stop(self.d);
            }
            self.speed = 0.0;
        } else {
            // Slow/fast sections: two slow-fast waves per lap, 30..230 px/s before SPEED.
            let u = self.d.rem_euclid(self.lap()) / self.lap();
            let cruise = (130.0 + 100.0 * (TAU * 2.0 * u).sin()) * SPEED;
            // Never faster than we can brake to the next stop, or than we can have sped up.
            let brake = (2.0 * ACCEL * (self.stop - self.d)).max(0.0).sqrt();
            let launch = START_SPEED + (2.0 * ACCEL * (self.d - self.leave)).sqrt();
            let v = cruise.min(brake).min(launch);
            let advance = v * dt as f64;
            if advance >= self.stop - self.d {
                self.d = self.stop;
                self.speed = 0.0;
                self.dwell = DWELL;
            } else {
                self.d += advance;
                self.speed = v as f32;
            }
        }
        self.pose()
    }

    fn pose(&self) -> Step {
        let t = self.t_at(self.d);
        // Heading = direction of travel = the curve's derivative.
        let (dx, dy) = (A * t.cos(), B * (2.0 * t).cos());
        Step {
            x: (A * t.sin()) as f32,
            y: (B / 2.0 * (2.0 * t).sin()) as f32,
            heading: dy.atan2(dx) as f32,
            speed: self.speed,
            // Phase depends only on distance walked: legs cycle faster when the fly runs and
            // freeze when it stops.
            gait_phase: (REST_PHASE + self.d / CYCLE_PX).rem_euclid(1.0) as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fly::{Feet, FlyPose, to_world};

    fn pose(s: &Step) -> FlyPose {
        FlyPose { x: s.x, y: s.y, heading: s.heading, speed: s.speed, gait_phase: s.gait_phase }
    }
    fn world(f: &Feet, s: &Step) -> Vec<(f32, f32)> {
        (0..6).map(|i| to_world(f.pos(i), &pose(s), FLY_SCALE)).collect()
    }

    /// Runs several laps at 60 fps (with an occasional dropped frame) and checks the feet
    /// against the real route:
    ///  1. a foot planted in two consecutive frames doesn't move on the screen,
    ///  2. whenever the fly stops, all six feet are planted and stay exactly where they are,
    ///  3. the fly does stop, about once per lap.
    #[test]
    fn no_skating_and_clean_stops() {
        let mut w = Walker::new();
        let mut prev = w.step(0.0);
        let mut feet = Feet::new(&pose(&prev), 1.0);
        let mut prev_world = world(&feet, &prev);
        let mut prev_planted: Vec<bool> = (0..6).map(|i| feet.planted(i)).collect();
        let (mut worst_slip, mut stops, mut was_stopped) = (0.0_f32, 0, false);
        for frame in 0..60 * 120 {
            let dt = if frame % 97 == 0 { 3.0 / 60.0 } else { 1.0 / 60.0 };
            let s = w.step(dt);
            feet.update(&pose(&s), 1.0);
            let wd = world(&feet, &s);
            for i in 0..6 {
                if prev_planted[i] && feet.planted(i) {
                    let slip = (wd[i].0 - prev_world[i].0).hypot(wd[i].1 - prev_world[i].1);
                    worst_slip = worst_slip.max(slip);
                }
            }
            if s.speed == 0.0 {
                assert!((0..6).all(|i| feet.planted(i)), "foot in the air while stopped");
                if !was_stopped {
                    stops += 1;
                }
                if prev.speed == 0.0 {
                    assert_eq!(wd, prev_world, "feet moved at rest");
                }
            }
            was_stopped = s.speed == 0.0;
            prev = s;
            prev_world = wd;
            prev_planted = (0..6).map(|i| feet.planted(i)).collect();
        }
        println!("worst planted-foot slip: {worst_slip} px/frame, stops: {stops}, lap {} px", w.lap());
        assert!(worst_slip < 0.01, "feet skate: {worst_slip} px in one frame");
        assert!((4..=8).contains(&stops), "expected ~1 stop/lap, got {stops}");
    }
}
