//! Auto-exposure for the auth feed: shorten the exposure when ambient
//! near-IR saturates the sensor, and only then.
//!
//! The production operating point (1 step, gain 128, exposure 1704) is a
//! dark-room point: the emitter is the light source and most of the frame is
//! dark. In direct sun (~3,500 lux) the same point puts 99.6 % of every frame
//! at full scale, emitter on or off, and howdy finds no face. Exposures of
//! 25..200 lines bring the face back (garden `projects/cement-ir/notes/007`).
//! Nothing upstream runs auto-exposure (no IPU3 IPA on this path), so the
//! feed does it, from the frames it is already unpacking.
//!
//! Three rules keep the dark-room path exactly as it was:
//!
//! - **The requested exposure is a ceiling.** The loop shortens the exposure
//!   and may lengthen it again, never past what the caller asked for.
//! - **It only shortens on a frame that is clipped AND ambient-lit**: the
//!   bright tail of the centre is at full scale, and less than half the frame
//!   is dark. A room lit by the emitter alone is mostly dark (92 % at the
//!   owner's usual distance, fid-gate Block 3), so it never qualifies.
//! - **It meters the centre**, where the face is, so a bright window behind
//!   the owner does not push the face into the dark.
//!
//! Frames, not the ambient light sensor: the ALS reads visible light, and LED
//! room light is bright to it but nearly invisible to this camera.
//!
//! A shorter exposure finds the face; it does not make a sunlit face match an
//! emitter-lit enrolment (notes/007: 97 % of the light was the sun's).

/// Never shorter than this many lines (~0.3 ms). At 12 the sunlit scene of
/// notes/007 had already lost the face.
pub const FLOOR: u32 = 16;
/// A centre p95 at or above this is clipped: the bright tail is at full scale.
pub const CLIPPED: u8 = 250;
/// A centre p95 below this has headroom, and the exposure may lengthen.
pub const LOW: u8 = 200;
/// Where a lengthening aims the centre p95.
pub const AIM: f64 = 230.0;
/// Whole-frame darkness (percent of pixels below 32) at or above which the
/// scene is the emitter's, and a clipped centre is left alone.
pub const EMITTER_LIT: f32 = 50.0;
/// Frames to skip after a change: the sensor applies a new exposure at a
/// frame boundary, and the frame already exposing still has the old one.
pub const SETTLE: u8 = 2;

/// What one frame says about its exposure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    /// Median and 95th percentile of the centre region.
    pub p50: u8,
    pub p95: u8,
    /// Whole-frame share of pixels below 32, in percent: the same darkness as
    /// `irfaced::stats`, on the frame as captured.
    pub darkness: f32,
}

/// The centre region: the middle half across, 15 %..85 % down. Where a face
/// in front of a tablet sits.
fn centre(width: usize, height: usize) -> (usize, usize, usize, usize) {
    (width / 4, width * 3 / 4, height * 15 / 100, height * 85 / 100)
}

/// Meter one `width` x `height` 8-bit frame.
pub fn meter(px: &[u8], width: usize, height: usize) -> Meter {
    debug_assert_eq!(px.len(), width * height);
    let (x0, x1, y0, y1) = centre(width, height);
    let mut hist = [0u32; 256];
    let mut dark = 0usize;
    for (y, row) in px.chunks_exact(width).enumerate() {
        dark += row.iter().filter(|&&p| p < 32).count();
        if (y0..y1).contains(&y) {
            for &p in &row[x0..x1] {
                hist[p as usize] += 1;
            }
        }
    }
    let n: u32 = hist.iter().sum();
    let pct = |q: f64| {
        let want = ((q * f64::from(n)).ceil() as u32).max(1);
        let mut acc = 0;
        for (v, c) in hist.iter().enumerate() {
            acc += c;
            if acc >= want {
                return v as u8;
            }
        }
        255
    };
    Meter {
        p50: pct(0.50),
        p95: pct(0.95),
        darkness: (dark as f64 * 100.0 / px.len().max(1) as f64) as f32,
    }
}

/// Why the exposure changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Half the centre at full scale: quarter the exposure.
    Blown,
    /// The bright tail at full scale: halve it.
    Clipped,
    /// Room below the aim, and below the ceiling: lengthen toward it.
    Headroom,
}

impl Why {
    pub fn word(self) -> &'static str {
        match self {
            Why::Blown => "blown",
            Why::Clipped => "clipped",
            Why::Headroom => "headroom",
        }
    }
}

/// The loop's state for one feed.
#[derive(Debug)]
pub struct Ae {
    ceiling: u32,
    exposure: u32,
    settle: u8,
    changes: u32,
}

impl Ae {
    /// Start at the requested exposure, which is also the ceiling.
    pub fn new(ceiling: u32) -> Ae {
        Ae { ceiling, exposure: ceiling, settle: 0, changes: 0 }
    }

    pub fn exposure(&self) -> u32 {
        self.exposure
    }

    pub fn changes(&self) -> u32 {
        self.changes
    }

    /// True while a change is still reaching the frames: skip metering. Call
    /// once per frame.
    pub fn settling(&mut self) -> bool {
        if self.settle > 0 {
            self.settle -= 1;
            true
        } else {
            false
        }
    }

    /// One metered frame in; the new exposure out when the sensor should
    /// change.
    pub fn step(&mut self, m: Meter) -> Option<(u32, Why)> {
        let (want, why) = if m.p95 >= CLIPPED && m.darkness < EMITTER_LIT {
            if m.p50 >= CLIPPED {
                (self.exposure / 4, Why::Blown)
            } else {
                (self.exposure / 2, Why::Clipped)
            }
        } else if m.p95 < LOW && self.exposure < self.ceiling {
            let f = (AIM / f64::from(m.p95.max(1))).min(2.0);
            ((f64::from(self.exposure) * f) as u32, Why::Headroom)
        } else {
            return None;
        };
        let e = want.clamp(FLOOR.min(self.ceiling), self.ceiling);
        if e == self.exposure {
            return None;
        }
        self.exposure = e;
        self.settle = SETTLE;
        self.changes += 1;
        Some((e, why))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 640;
    const H: usize = 480;

    /// A linear scene: each pixel's level is radiance x exposure, clipped at
    /// 255. `centre` spans the face region from its first to its last value,
    /// `back` is everything else.
    struct Scene {
        centre: (f64, f64),
        back: f64,
    }

    impl Scene {
        fn frame(&self, exposure: u32) -> Vec<u8> {
            let (x0, x1, y0, y1) = centre(W, H);
            let e = f64::from(exposure);
            let mut px = vec![0u8; W * H];
            for y in 0..H {
                for x in 0..W {
                    let r = if (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                        let t = (x - x0) as f64 / (x1 - x0 - 1) as f64;
                        self.centre.0 + t * (self.centre.1 - self.centre.0)
                    } else {
                        self.back
                    };
                    px[y * W + x] = (r * e).round().min(255.0) as u8;
                }
            }
            px
        }
    }

    /// Run the loop for `frames` frames; the exposure each frame was taken at.
    fn run(ae: &mut Ae, scene: &Scene, frames: usize) -> Vec<u32> {
        let mut seen = Vec::new();
        for _ in 0..frames {
            let e = ae.exposure();
            seen.push(e);
            let px = scene.frame(e);
            if !ae.settling() {
                ae.step(meter(&px, W, H));
            }
        }
        seen
    }

    // Direct sun (notes/007): everything clipped at 1704, the face found at
    // 25..200. Radiances put the centre at full scale from ~250 lines up.
    const SUN: Scene = Scene { centre: (0.4, 1.1), back: 0.6 };
    // The emitter alone: the background is black, the face is lit and its
    // bright tail clips at the production point.
    const EMITTER: Scene = Scene { centre: (0.05, 0.2), back: 0.0 };

    #[test]
    fn meter_reads_the_centre_and_the_whole_frame() {
        let m = meter(&EMITTER.frame(1704), W, H);
        assert_eq!(m.p95, 255, "the emitter-lit face clips at 1704");
        // Only the centre is lit: 50 % x 70 % = 35 % of the frame.
        assert!((m.darkness - 65.0).abs() < 2.0, "darkness {}", m.darkness);
        let m = meter(&vec![255; W * H], W, H);
        assert_eq!((m.p50, m.p95, m.darkness), (255, 255, 0.0));
    }

    #[test]
    fn the_dark_room_point_is_never_touched() {
        let mut ae = Ae::new(1704);
        let seen = run(&mut ae, &EMITTER, 60);
        assert!(seen.iter().all(|&e| e == 1704), "{seen:?}");
        assert_eq!(ae.changes(), 0);
    }

    #[test]
    fn sun_converges_unclipped_and_stays() {
        let mut ae = Ae::new(1704);
        let seen = run(&mut ae, &SUN, 60);
        let settled = *seen.last().unwrap();
        assert!(
            seen[30..].iter().all(|&e| e == settled),
            "still moving after 30 frames: {seen:?}"
        );
        let m = meter(&SUN.frame(settled), W, H);
        assert!(m.p95 >= LOW && m.p95 < CLIPPED, "p95 {} at {settled}", m.p95);
        assert!(ae.changes() <= 6, "{} changes: {seen:?}", ae.changes());
        // Within howdy's first second: 30 fps.
        let first = seen.iter().position(|&e| e == settled).unwrap();
        assert!(first <= 20, "settled at frame {first}: {seen:?}");
    }

    /// Leaving the sun mid-feed lengthens the exposure again until the face's
    /// bright tail is back near the aim. It need not reach the ceiling: the
    /// next feed starts there anyway, because every feed starts a new loop.
    #[test]
    fn leaving_the_sun_lengthens_again() {
        let mut ae = Ae::new(1704);
        run(&mut ae, &SUN, 40);
        let sunlit = ae.exposure();
        assert!(sunlit < 400, "{sunlit}");
        let seen = run(&mut ae, &EMITTER, 40);
        let e = *seen.last().unwrap();
        let m = meter(&EMITTER.frame(e), W, H);
        assert!(e > 2 * sunlit, "{seen:?}");
        assert!(e == 1704 || (m.p95 >= LOW && m.p95 < CLIPPED), "p95 {} at {e}", m.p95);
        assert!(seen[30..].iter().all(|&x| x == e), "still moving: {seen:?}");
    }

    #[test]
    fn a_lamp_in_the_lens_stops_at_the_floor() {
        let mut ae = Ae::new(1704);
        let lamp = Scene { centre: (50.0, 60.0), back: 40.0 };
        let seen = run(&mut ae, &lamp, 60);
        assert_eq!(*seen.last().unwrap(), FLOOR, "{seen:?}");
    }

    #[test]
    fn never_past_the_ceiling_or_below_a_low_ceiling() {
        // A caller asking for less than the floor gets exactly what it asked.
        let mut ae = Ae::new(10);
        assert_eq!(run(&mut ae, &SUN, 30).iter().max(), Some(&10));
        assert_eq!(ae.exposure(), 10);
        // A dim ambient scene below the ceiling only climbs back to it.
        let mut ae = Ae::new(1704);
        let dim = Scene { centre: (0.01, 0.02), back: 0.5 };
        assert!(run(&mut ae, &dim, 30).iter().all(|&e| e == 1704));
    }

    #[test]
    fn settling_skips_exactly_settle_frames() {
        let mut ae = Ae::new(1704);
        let blown = meter(&vec![255; W * H], W, H);
        assert_eq!(ae.step(blown), Some((426, Why::Blown)));
        for _ in 0..SETTLE {
            assert!(ae.settling());
        }
        assert!(!ae.settling());
    }
}
