//! Tape/transport simulation: tracking error, vertical-hold roll, dropouts.
//!
//! The roll works on a 525-line raster whose 8.5% blanking interval is normally hidden by overscan.
//! `roll` offsets the raster; when it is non-zero the blanking bar scrolls into view.
//! State machine: Locked -> Rolling (free-running) -> Catch (damped spring recapture, so it bounces) -> Locked.
use crate::params::{Cond, Knobs, Mode, Speed};
use crate::rng::Rng;
use crate::smooth;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RollState { Locked, Rolling, Catch }

#[derive(Clone, Copy, Debug)]
pub struct Dropout {
    pub line: f32,
    pub x0: f32,
    pub len: f32,
    /// positive = white fleck, negative = black fleck
    pub strength: f32,
    pub ttl: f32,
}

pub struct SimCtx {
    pub mode: Mode,
    pub cond: Cond,
    pub speed: Speed,
    pub knobs: Knobs,
}

/// Everything the shader needs for one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct View {
    pub blue_screen: f32,
    pub roll: f32,
    pub flag: f32,
    pub luma: f32,
    pub chroma: f32,
    pub noise: f32,
    pub head: f32,
    pub color: f32,
    pub jitter: f32,
    pub snr: f32,
    pub band_pos: f32,
    pub band_h: f32,
    pub band_str: f32,
    pub band_n: f32,
    pub track_err: f32,
    pub drops: [[f32; 4]; 8],
}

pub struct Sim {
    t: f32,
    roll: f32,
    v: f32,
    state: RollState,
    timer: f32,
    target: f32,
    flag: f32,
    opt: f32,
    err: f32,
    abs_e: f32,
    band_pos: f32,
    drops: Vec<Dropout>,
    force: bool,
    rng: Rng,
}

impl Sim {
    pub fn new(seed: u64) -> Self {
        Sim {
            t: 0.0, roll: 0.0, v: 0.0, state: RollState::Locked, timer: 0.0, target: 0.0, flag: 0.0,
            opt: 0.0, err: 0.0, abs_e: 0.0, band_pos: 0.4, drops: Vec::new(), force: false, rng: Rng::new(seed),
        }
    }

    pub fn force_roll(&mut self) { self.force = true; }
    pub fn state(&self) -> RollState { self.state }
    pub fn roll_offset(&self) -> f32 { self.roll }
    /// Magnitude of the tracking error (0 = perfectly tracked). Also drives audio buzz.
    pub fn track_error(&self) -> f32 { self.abs_e }
    /// Where the deck's tracking *should* be. Hidden from the user; AUTO follows it.
    /// Signed tracking error (knob minus optimum).
    pub fn track_signed(&self) -> f32 { self.err }
    pub fn optimum(&self) -> f32 { self.opt }

    fn start_roll(&mut self, cond: Cond) {
        self.state = RollState::Rolling;
        let sign = if self.rng.f32() < 0.5 { -1.0 } else { 1.0 };
        self.v = sign * (0.4 + self.rng.f32() * 1.4);
        self.timer = 0.5 + self.rng.f32() * (1.2 + cond.spec().roll * 20.0);
    }

    /// Advance the simulation. Returns audio dropout durations (seconds) to trigger this frame.
    /// `track` is the user's tracking position in [-1, 1]; with `auto` it is pulled toward the optimum.
    pub fn update(&mut self, dt: f32, ctx: &SimCtx, track: &mut f32, auto: bool) -> Vec<f32> {
        let dt = dt.clamp(0.0, 0.05);
        let c = ctx.cond.spec();
        let playing = ctx.mode == Mode::Play;
        self.t += dt;
        self.opt = c.trk * (0.55 * (self.t * 0.07).sin() + 0.45 * (self.t * 0.19 + 1.3).sin());
        if auto {
            *track += (self.opt - *track) * (dt * 1.2).min(1.0);
        }
        self.err = *track - self.opt;
        self.abs_e = self.err.abs();

        // vertical hold
        if self.state == RollState::Locked && playing {
            let rate = c.roll * if ctx.speed == Speed::Ep { 1.5 } else { 1.0 } + smooth(0.55, 1.0, self.abs_e) * 0.8;
            if self.force || self.rng.f32() < rate * dt {
                self.start_roll(ctx.cond);
            } else if self.rng.f32() < c.bounce * dt {
                self.v += (self.rng.f32() - 0.5) * 0.25;
            }
        }
        self.force = false;
        if !playing && self.state == RollState::Rolling {
            self.state = RollState::Catch;
            self.target = self.roll.round();
        }
        if self.state == RollState::Rolling {
            self.roll += self.v * dt;
            self.v += (self.rng.f32() - 0.5) * dt * 0.6;
            self.timer -= dt;
            if self.timer <= 0.0 {
                self.state = RollState::Catch;
                self.target = (self.roll + self.v * 0.22).round();
            }
        } else {
            let catching = self.state == RollState::Catch;
            let (k, c2) = if catching { (36.0, 6.5) } else { (300.0, 30.0) };
            self.v += (-k * (self.roll - self.target) - c2 * self.v) * dt;
            self.roll += self.v * dt;
            if catching && (self.roll - self.target).abs() < 0.0015 && self.v.abs() < 0.02 {
                self.roll -= self.target;
                self.target = 0.0;
                self.state = RollState::Locked;
            }
        }
        let flag_t = match self.state {
            RollState::Rolling => 1.0,
            RollState::Catch => (self.v.abs() * 1.5).clamp(0.0, 1.0),
            RollState::Locked => 0.0,
        };
        self.flag += (flag_t - self.flag) * (dt * 12.0).min(1.0);

        // dropouts live on the tape, shared by video and audio
        for d in &mut self.drops { d.ttl -= dt; }
        self.drops.retain(|d| d.ttl > 0.0);
        let mut audio = Vec::new();
        if playing {
            let mut p = c.drop * (0.5 + ctx.knobs.noise) * (1.0 + self.abs_e * 3.0) * ctx.speed.spec().snr * dt;
            while p > 0.0 {
                if self.rng.f32() < p.min(1.0) {
                    let sign = if self.rng.f32() < 0.85 { 1.0 } else { -1.0 };
                    self.drops.push(Dropout {
                        line: self.rng.f32() * 525.0,
                        x0: self.rng.f32() * 0.9,
                        len: 0.015 + self.rng.f32().powf(2.5) * 0.35,
                        strength: sign * (0.6 + 0.4 * self.rng.f32()),
                        ttl: 0.03 + self.rng.f32() * 0.06,
                    });
                    if self.drops.len() > 8 { self.drops.remove(0); }
                    if self.rng.f32() < 0.3 + self.abs_e {
                        audio.push(0.02 + self.rng.f32() * 0.05);
                    }
                }
                p -= 1.0;
            }
        }
        audio
    }

    /// Build the per-frame shader parameters. Advances the tracking-band position.
    pub fn view(&mut self, dt: f32, ctx: &SimCtx) -> View {
        let dt = dt.clamp(0.0, 0.05);
        let sp = ctx.speed.spec();
        let c = ctx.cond.spec();
        let k = &ctx.knobs;
        let (mut band_str, mut band_h, mut band_n, mut err) = (0.0, 0.05, 1.0, 0.0);
        let mut jit = sp.jit * c.jit + self.flag * 3.0 + self.abs_e * 3.0;
        match ctx.mode {
            Mode::Play => {
                band_str = smooth(0.06, 0.55, self.abs_e);
                band_h = 0.03 + 0.12 * self.abs_e;
                err = self.abs_e;
                self.band_pos += dt * (0.05 + 0.25 * self.abs_e) * if self.err < 0.0 { -1.0 } else { 1.0 };
            }
            Mode::Pause => { band_str = 0.55; jit += 1.5; self.band_pos += dt * 0.02; }
            Mode::Ff | Mode::Rew => {
                band_n = 3.0; band_str = 1.0; band_h = 0.07; err = 0.6; jit += 1.5;
                self.band_pos += if ctx.mode == Mode::Ff { -1.0 } else { 1.0 } * dt * 1.2;
            }
            _ => {}
        }
        self.band_pos -= self.band_pos.floor();

        let mut drops = [[0.0f32; 4]; 8];
        for (i, d) in self.drops.iter().take(8).enumerate() {
            drops[i] = [d.line, d.x0, d.len, d.strength];
        }
        View {
            blue_screen: if matches!(ctx.mode, Mode::None | Mode::Stop) { 1.0 } else { 0.0 },
            roll: self.roll.rem_euclid(1.0),
            flag: self.flag,
            luma: sp.luma,
            chroma: sp.chroma,
            noise: k.noise,
            head: k.head,
            color: k.color,
            jitter: jit,
            snr: sp.snr,
            band_pos: self.band_pos,
            band_h,
            band_str,
            band_n,
            track_err: err,
            drops,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(cond: Cond) -> SimCtx {
        SimCtx { mode: Mode::Play, cond, speed: Speed::Sp, knobs: Knobs::preset(cond) }
    }

    #[test]
    fn good_tape_with_good_tracking_stays_locked() {
        let mut s = Sim::new(1);
        let mut trk = 0.0;
        for _ in 0..600 { s.update(1.0 / 60.0, &ctx(Cond::Good), &mut trk, true); }
        // 10 s of a good tape on auto tracking: tracking error stays tiny
        assert!(s.track_error() < 0.05);
    }

    #[test]
    fn forced_roll_scrolls_then_recaptures() {
        let mut s = Sim::new(7);
        let mut trk = 0.0;
        let c = ctx(Cond::Worn);
        s.force_roll();
        s.update(1.0 / 60.0, &c, &mut trk, false);
        assert_eq!(s.state(), RollState::Rolling);
        let mut max_off = 0.0f32;
        for _ in 0..60 * 20 {
            s.update(1.0 / 60.0, &c, &mut trk, false);
            max_off = max_off.max(s.roll_offset().abs());
            if s.state() == RollState::Locked { break; }
        }
        assert_eq!(s.state(), RollState::Locked, "must recapture sync");
        assert!(max_off > 0.2, "picture should actually have moved, got {max_off}");
        // after locking, the offset settles near an integer number of rasters (view wraps it to ~0)
        for _ in 0..120 { s.update(1.0 / 60.0, &c, &mut trk, false); }
        let r = s.roll_offset();
        assert!(r.abs() < 0.01, "locked roll offset {r}");
    }

    #[test]
    fn bad_tracking_causes_rolls_and_noise_band() {
        let mut s = Sim::new(3);
        let mut trk = 0.95;
        let c = ctx(Cond::Good);
        let mut rolled = false;
        for _ in 0..60 * 30 {
            s.update(1.0 / 60.0, &c, &mut trk, false);
            if s.state() != RollState::Locked { rolled = true; }
        }
        assert!(rolled, "tracking far from optimum should lose vertical hold");
        let v = s.view(1.0 / 60.0, &c);
        assert!(v.band_str > 0.9);
    }

    #[test]
    fn damaged_tape_produces_dropouts_good_tape_rarely() {
        let count = |cond| {
            let mut s = Sim::new(5);
            let mut trk = 0.0;
            let mut n = 0;
            for _ in 0..60 * 20 { n += s.update(1.0 / 60.0, &ctx(cond), &mut trk, true).len(); }
            n
        };
        assert!(count(Cond::Damaged) > count(Cond::Good) * 3);
    }

    #[test]
    fn stopped_deck_shows_blue_screen() {
        let mut s = Sim::new(1);
        let mut c = ctx(Cond::Good);
        c.mode = Mode::Stop;
        assert_eq!(s.view(0.016, &c).blue_screen, 1.0);
    }
}
