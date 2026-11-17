//! Tape audio chain:
//! hi-pass -> wow/flutter modulated delay -> two low-pass stages (tape bandwidth) -> saturation
//! -> dropout gain -> + hiss + tracking buzz -> mute/volume.
use crate::params::{audio_cutoff, Knobs, Speed};
use crate::rng::Rng;
use std::f32::consts::TAU;

#[derive(Clone, Copy, Default)]
pub struct Biquad { b0: f32, b1: f32, b2: f32, a1: f32, a2: f32, z1: f32, z2: f32 }

impl Biquad {
    fn coeffs(sr: f32, f: f32, q: f32) -> (f32, f32, f32) {
        let w0 = TAU * f.clamp(20.0, sr * 0.45) / sr;
        let (s, c) = w0.sin_cos();
        (c, s / (2.0 * q), 1.0 + s / (2.0 * q))
    }
    pub fn lowpass(sr: f32, f: f32, q: f32) -> Self { let mut b = Biquad::default(); b.set_lowpass(sr, f, q); b }
    pub fn highpass(sr: f32, f: f32, q: f32) -> Self { let mut b = Biquad::default(); b.set_highpass(sr, f, q); b }
    pub fn bandpass(sr: f32, f: f32, q: f32) -> Self { let mut b = Biquad::default(); b.set_bandpass(sr, f, q); b }
    pub fn set_lowpass(&mut self, sr: f32, f: f32, q: f32) {
        let (c, alpha, a0) = Self::coeffs(sr, f, q);
        self.b0 = (1.0 - c) / 2.0 / a0; self.b1 = (1.0 - c) / a0; self.b2 = self.b0;
        self.a1 = -2.0 * c / a0; self.a2 = (1.0 - alpha) / a0;
    }
    pub fn set_highpass(&mut self, sr: f32, f: f32, q: f32) {
        let (c, alpha, a0) = Self::coeffs(sr, f, q);
        self.b0 = (1.0 + c) / 2.0 / a0; self.b1 = -(1.0 + c) / a0; self.b2 = self.b0;
        self.a1 = -2.0 * c / a0; self.a2 = (1.0 - alpha) / a0;
    }
    pub fn set_bandpass(&mut self, sr: f32, f: f32, q: f32) {
        let (c, alpha, a0) = Self::coeffs(sr, f, q);
        self.b0 = alpha / a0; self.b1 = 0.0; self.b2 = -alpha / a0;
        self.a1 = -2.0 * c / a0; self.a2 = (1.0 - alpha) / a0;
    }
    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

#[derive(Clone, Copy)]
pub struct AudioParams {
    pub knobs: Knobs,
    pub speed: Speed,
    /// |tracking error| from the simulation; drives the warble/fade of a misaligned audio head
    pub track_err: f32,
    /// false = pause/stop/search: tape output is muted (as on a real deck)
    pub playing: bool,
    pub volume: f32,
}

const WANDER: [f32; 9] = [1.0, 2.0, 3.0, 5.0, 7.0, 9.0, 13.0, 17.0, 23.0];
const MAX_DELAY_S: f32 = 0.1;
const BASE_DELAY_S: f32 = 0.03;

struct Chan {
    hp: Biquad, lp1: Biquad, lp2: Biquad,
    hiss_hp: Biquad, hiss_lp: Biquad,
    line: Vec<f32>,
}

pub struct TapeAudio {
    sr: f32,
    ch: [Chan; 2],
    w: usize,
    rng: Rng,
    // modulators
    fld_ph: f32, tgt_g: f32, cur_g: f32, tgt_c: f32, cur_c: f32, wow_ph: f32, fl1_ph: f32, fl2_ph: f32, t: f32,
    wander_ph: [f32; 9], wander_norm: f32,
    // smoothed controls
    cutoff: f32, hiss: f32, wow_g: f32, fl1_g: f32, fl2_g: f32, wander_g: f32, mis: f32, vol: f32, mute: f32,
    // dropout envelope
    drop_age: f32, drop_len: f32, drop_start: f32, drop_active: bool, drop_gain: f32,
    sat_k: f32, sat_norm: f32,
}

impl TapeAudio {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate;
        let mut rng = Rng::new(0xA0D10);
        let mk = || Chan {
            hp: Biquad::highpass(sr, 45.0, 0.7), lp1: Biquad::lowpass(sr, 9000.0, 0.9), lp2: Biquad::lowpass(sr, 9000.0, 0.6),
            hiss_hp: Biquad::highpass(sr, 1500.0, 0.7), hiss_lp: Biquad::lowpass(sr, 11000.0, 0.7),
            line: vec![0.0; (MAX_DELAY_S * sr) as usize + 8],
        };
        let mut wander_ph = [0.0; 9];
        for p in &mut wander_ph { *p = rng.f32() * TAU; }
        let wander_norm = WANDER.iter().map(|n| 1.0 / n.sqrt()).sum::<f32>() * 0.6;
        let k = 1.5f32;
        TapeAudio {
            sr, ch: [mk(), mk()], w: 0, rng,
            fld_ph: 0.0, tgt_g: 1.0, cur_g: 1.0, tgt_c: 1.0, cur_c: 1.0, wow_ph: 0.0, fl1_ph: 0.0, fl2_ph: 0.0, t: 0.0, wander_ph, wander_norm,
            cutoff: 9000.0, hiss: 0.0, wow_g: 0.0, fl1_g: 0.0, fl2_g: 0.0, wander_g: 0.0, mis: 0.0, vol: 0.64, mute: 0.0,
            drop_age: 0.0, drop_len: 0.0, drop_start: 1.0, drop_active: false, drop_gain: 1.0,
            sat_k: k, sat_norm: 1.0 / k.tanh(),
        }
    }

    /// Short loss of signal (a tape dropout). `dur` is the recovery time in seconds.
    pub fn dropout(&mut self, dur: f32) {
        self.drop_start = self.drop_gain;
        self.drop_age = 0.0;
        self.drop_len = dur;
        self.drop_active = true;
    }

    /// Process interleaved stereo in place.
    pub fn process(&mut self, buf: &mut [f32], p: &AudioParams) {
        let frames = buf.len() / 2;
        if frames == 0 { return; }
        let sr = self.sr;
        let block_dt = frames as f32 / sr;
        let a = 1.0 - (-block_dt / 0.06).exp(); // one-pole smoothing toward the targets
        let sp = p.speed.spec();
        let k = &p.knobs;
        let w = k.wow * sp.wow;
        let slew = |cur: &mut f32, target: f32| *cur += (target - *cur) * a;
        slew(&mut self.cutoff, audio_cutoff(k.bw, p.speed));
        slew(&mut self.hiss, k.hiss.powf(1.4) * 0.06 * sp.hiss);
        slew(&mut self.wow_g, w * 0.0025);
        slew(&mut self.fl1_g, w * 0.00012);
        slew(&mut self.fl2_g, w * 0.00007);
        slew(&mut self.wander_g, w * 0.004);
        slew(&mut self.mis, crate::smooth(0.12, 0.9, p.track_err));
        let vol_t = p.volume * p.volume;
        slew(&mut self.vol, vol_t);
        let mute_t = if p.playing { 1.0 } else { 0.0 };
        // tracking misalignment: handled per field below; the cutoff follows the head's grip on the track
        let cut = self.cutoff * self.cur_c.max(0.2);
        for c in &mut self.ch {
            c.lp1.set_lowpass(sr, cut, 0.9);
            c.lp2.set_lowpass(sr, cut * 1.05, 0.6);
        }
        let n = self.ch[0].line.len();
        for i in 0..frames {
            self.wow_ph = (self.wow_ph + TAU * 0.83 / sr) % TAU;
            self.fl1_ph = (self.fl1_ph + TAU * 9.3 / sr) % TAU;
            self.fl2_ph = (self.fl2_ph + TAU * 13.1 / sr) % TAU;
            self.t = (self.t + 1.0 / sr) % 8.0;
            let mut wander = 0.0;
            for (j, f) in WANDER.iter().enumerate() {
                wander += (TAU * f / 8.0 * self.t + self.wander_ph[j]).sin() / f.sqrt();
            }
            wander /= self.wander_norm;
            let md = self.wow_g * self.wow_ph.sin() + self.fl1_g * self.fl1_ph.sin()
                + self.fl2_g * self.fl2_ph.sin() + self.wander_g * wander;
            let m = self.mis;
            // misaligned heads: every video field (59.94 Hz) the audio head either picks the track up or
            // loses it, so the sound chops in and out at the field rate (no pitch change, no added noise)
            self.fld_ph += 59.94 / sr;
            if self.fld_ph >= 1.0 {
                self.fld_ph -= 1.0;
                let lose = self.rng.f32() < 0.25 + 0.6 * m;
                let depth = 0.2 + 0.8 * self.rng.f32();
                self.tgt_g = if lose { 1.0 - m * depth } else { 1.0 };
                self.tgt_c = if lose { 1.0 - 0.7 * m * depth } else { 1.0 };
            }
            let sl = 1.0 - (-1.0 / (0.0012 * sr)).exp();
            self.cur_g += (self.tgt_g - self.cur_g) * sl;
            self.cur_c += (self.tgt_c - self.cur_c) * sl;
            let dly = ((BASE_DELAY_S + md) * sr).clamp(1.0, (n - 4) as f32);

            // dropout envelope: fast drop to 12%, then recover linearly
            if self.drop_active {
                self.drop_age += 1.0 / sr;
                self.drop_gain = if self.drop_age < 0.006 {
                    self.drop_start + (0.12 - self.drop_start) * self.drop_age / 0.006
                } else {
                    let r = (self.drop_age - 0.006) / self.drop_len.max(1e-3);
                    if r >= 1.0 { self.drop_active = false; 1.0 } else { 0.12 + 0.88 * r }
                };
            }
            self.mute += (mute_t - self.mute) * 0.002;
            let out_g = self.mute * self.vol * self.cur_g;

            let rpos = self.w as f32 - dly;
            let rpos = if rpos < 0.0 { rpos + n as f32 } else { rpos };
            let i0 = rpos.floor() as usize % n;
            let i1 = (i0 + 1) % n;
            let fr = rpos - rpos.floor();
            for (chn, c) in self.ch.iter_mut().enumerate() {
                let x = c.hp.tick(buf[i * 2 + chn]);
                c.line[self.w] = x;
                let y = c.line[i0] * (1.0 - fr) + c.line[i1] * fr;
                let y = c.lp2.tick(c.lp1.tick(y));
                let y = (self.sat_k * y).tanh() * self.sat_norm * self.drop_gain;
                let noise = self.rng.bipolar();
                let hiss = c.hiss_lp.tick(c.hiss_hp.tick(noise)) * self.hiss;
                buf[i * 2 + chn] = (y + hiss) * out_g;
            }
            self.w = (self.w + 1) % n;
        }
        let _ = (&p.knobs, Speed::Sp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn params(knobs: Knobs) -> AudioParams {
        AudioParams { knobs, speed: Speed::Sp, track_err: 0.0, playing: true, volume: 1.0 }
    }
    fn rms(x: &[f32]) -> f32 { (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt() }
    fn sine(f: f32, n: usize) -> Vec<f32> {
        (0..n).flat_map(|i| { let s = (TAU * f * i as f32 / SR).sin() * 0.5; [s, s] }).collect()
    }
    /// run `secs` of input through a fresh chain in 512-frame blocks, return the last `keep` seconds
    fn run(mut input: Vec<f32>, p: AudioParams, keep: f32) -> Vec<f32> {
        let mut t = TapeAudio::new(SR);
        for blk in input.chunks_mut(1024) { t.process(blk, &p); }
        let k = (keep * SR) as usize * 2;
        input[input.len() - k..].to_vec()
    }
    fn knobs(bw: f32, hiss: f32, wow: f32) -> Knobs { Knobs { noise: 0.0, head: 0.0, color: 0.0, hiss, wow, bw } }

    #[test]
    fn bandwidth_knob_muffles_high_frequencies() {
        let wide = rms(&run(sine(8000.0, 48000), params(knobs(1.0, 0.0, 0.0)), 0.5));
        let narrow = rms(&run(sine(8000.0, 48000), params(knobs(0.0, 0.0, 0.0)), 0.5));
        assert!(wide > 0.1, "wide {wide}");
        assert!(narrow < wide * 0.02, "narrow {narrow} wide {wide}");
    }

    #[test]
    fn slower_tape_speed_is_more_muffled() {
        let mut p = params(knobs(0.7, 0.0, 0.0));
        let sp = rms(&run(sine(5000.0, 48000), p, 0.5));
        p.speed = Speed::Ep;
        let ep = rms(&run(sine(5000.0, 48000), p, 0.5));
        assert!(ep < sp * 0.5, "ep {ep} sp {sp}");
    }

    #[test]
    fn hiss_adds_noise_floor_to_silence() {
        let quiet = rms(&run(vec![0.0; 48000 * 2], params(knobs(1.0, 0.0, 0.0)), 0.5));
        let hissy = rms(&run(vec![0.0; 48000 * 2], params(knobs(1.0, 1.0, 0.0)), 0.5));
        assert!(quiet < 1e-4, "quiet {quiet}");
        assert!(hissy > 0.005, "hissy {hissy}");
    }

    #[test]
    fn wow_and_flutter_modulate_pitch() {
        // zero-crossing spacing of a 1 kHz tone varies when wow & flutter is up
        let spread = |wow: f32| {
            let out = run(sine(1000.0, 48000 * 4), params(knobs(1.0, 0.0, wow)), 3.0);
            let l: Vec<f32> = out.iter().step_by(2).copied().collect();
            let mut zc = Vec::new();
            for i in 1..l.len() { if l[i - 1] < 0.0 && l[i] >= 0.0 { zc.push(i as f32); } }
            let d: Vec<f32> = zc.windows(2).map(|w| w[1] - w[0]).collect();
            let mean = d.iter().sum::<f32>() / d.len() as f32;
            (d.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / d.len() as f32).sqrt() / mean
        };
        let steady = spread(0.0);
        let wobbly = spread(1.0);
        assert!(steady < 0.002, "steady {steady}");
        assert!(wobbly > steady * 5.0 && wobbly > 0.003, "wobbly {wobbly} steady {steady}");
    }

    #[test]
    fn tracking_misalignment_chops_the_sound_without_noise_or_pitch_change() {
        let mut p = params(knobs(1.0, 0.0, 0.0));
        p.track_err = 1.0;
        let silent = rms(&run(vec![0.0; 48000 * 2], p, 0.5));
        assert!(silent < 1e-4, "misalignment must not add noise: {silent}");
        let out = run(sine(1000.0, 48000 * 4), p, 3.0);
        let l: Vec<f32> = out.iter().step_by(2).copied().collect();
        let wins: Vec<f32> = l.chunks(2400).map(rms).collect();
        let (lo, hi) = (wins.iter().cloned().fold(f32::MAX, f32::min), wins.iter().cloned().fold(0.0, f32::max));
        assert!(lo < hi * 0.6, "level should chop in and out: {lo} {hi}");
        // pitch must stay put: no helium. Count zero crossings in the loud windows.
        let mut zc = 0; for i in 1..l.len() { if l[i - 1] < 0.0 && l[i] >= 0.0 { zc += 1; } }
        let hz = zc as f32 / 3.0;
        assert!((hz - 1000.0).abs() < 60.0, "pitch drifted: {hz} Hz");
    }

    #[test]
    fn pause_mutes_everything_and_dropout_dips_the_signal() {
        let mut p = params(knobs(1.0, 1.0, 0.0));
        p.playing = false;
        assert!(rms(&run(sine(440.0, 48000), p, 0.2)) < 1e-4);

        let p = params(knobs(1.0, 0.0, 0.0));
        let mut t = TapeAudio::new(SR);
        let mut settle = sine(440.0, 24000);
        for b in settle.chunks_mut(1024) { t.process(b, &p); }
        let before = rms(&settle[settle.len() - 4096..]);
        t.dropout(0.05);
        let mut dip = sine(440.0, 2400); // 50 ms
        t.process(&mut dip, &p);
        let min_win = dip.chunks(480).map(rms).fold(f32::MAX, f32::min);
        assert!(min_win < before * 0.3, "dropout depth: {min_win} vs {before}");
    }
}
