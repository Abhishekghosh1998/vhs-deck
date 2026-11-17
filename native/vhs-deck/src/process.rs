//! The picture path: the real ntsc-rs effect (vendored `ntscrs` crate) driven by the deck controls,
//! plus the tape-only artefacts ntsc-rs does not model (dropouts, sync flagging).
use crate::media::FrameData;
use ntsc_rs::settings::standard::{TrackingBar, TrackingLayout, UseField, VHSTapeSpeed};
use ntsc_rs::yiq_fielding::Rgbx;
use ntsc_rs::NtscEffect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use vhs_core::params::{Cond, Speed};

// ------------------------------------------------------------------------ deck knobs <-> ntsc-rs fields
// The simple VCR sliders read and write the real ntsc-rs settings, so they always agree with the
// advanced panel (which shows every ntsc-rs setting).

pub fn get_noise(e: &NtscEffect) -> f32 { (e.composite_noise.settings.intensity / 0.25).clamp(0.0, 1.0) }
pub fn set_noise(e: &mut NtscEffect, k: f32) {
    e.composite_noise.enabled = k > 0.001;
    e.composite_noise.settings.intensity = k * 0.25;
    e.luma_noise.enabled = k > 0.001;
    e.luma_noise.settings.intensity = k * 0.04;
    e.snow_intensity = 0.00025 + k * k * 0.004;
}

pub fn get_head(e: &NtscEffect) -> f32 {
    if e.head_switching.enabled { (e.head_switching.settings.horiz_shift / 90.0).clamp(0.0, 1.0) } else { 0.0 }
}
pub fn set_head(e: &mut NtscEffect, k: f32) {
    e.head_switching.enabled = k > 0.005;
    e.head_switching.settings.height = (4.0 + k * 8.0).round() as i32;
    e.head_switching.settings.horiz_shift = k * 90.0;
}

pub fn get_colour(e: &NtscEffect) -> f32 { if e.chroma_noise.enabled { (e.chroma_noise.settings.intensity / 0.4).clamp(0.0, 1.0) } else { 0.0 } }
pub fn set_colour(e: &mut NtscEffect, k: f32) {
    e.chroma_noise.enabled = k > 0.001;
    e.chroma_noise.settings.intensity = k * 0.4;
    e.chroma_phase_noise_intensity = k * 0.04;
    e.chroma_phase_error = k * 0.1;
    e.chroma_delay_horizontal = k * 3.0;
    e.vhs_settings.enabled = true;
    e.vhs_settings.settings.chroma_loss = 0.000025 + k * k * 0.002;
}

pub fn speed_of(e: &NtscEffect) -> Speed {
    match e.vhs_settings.settings.tape_speed {
        VHSTapeSpeed::LP => Speed::Lp,
        VHSTapeSpeed::EP => Speed::Ep,
        _ => Speed::Sp,
    }
}
pub fn set_speed(e: &mut NtscEffect, s: Speed) {
    e.vhs_settings.enabled = true;
    e.vhs_settings.settings.tape_speed = match s { Speed::Sp => VHSTapeSpeed::SP, Speed::Lp => VHSTapeSpeed::LP, Speed::Ep => VHSTapeSpeed::EP };
}

/// Tape condition = a starting point for the ntsc-rs settings (which can then be tweaked freely).
pub fn preset(c: Cond) -> NtscEffect {
    let mut e = NtscEffect::default();
    e.use_field = UseField::InterleavedUpper;
    let (noise, head, colour, wave, sharpen) = match c {
        Cond::Good => (0.12, 0.30, 0.10, 0.25, 0.25),
        Cond::Worn => (0.35, 0.60, 0.40, 0.70, 0.35),
        Cond::Damaged => (0.70, 0.90, 0.75, 1.40, 0.45),
    };
    set_speed(&mut e, Speed::Sp);
    set_noise(&mut e, noise);
    set_head(&mut e, head);
    set_colour(&mut e, colour);
    e.vhs_settings.settings.edge_wave.enabled = true;
    e.vhs_settings.settings.edge_wave.settings.intensity = wave;
    e.vhs_settings.settings.sharpen.settings.intensity = sharpen;
    // tracking noise: its look comes from the ntsc-rs settings, the TRACKING control moves and grows it
    e.tracking_noise.enabled = true;
    e
}

// ------------------------------------------------------------------------ tape artefacts on the processed frame

/// Tape dropouts: short horizontal streaks, `drops[i] = [raster line (of 525), x0, length, strength]`
/// (strength > 0 white, < 0 black, 0 = unused). Positions are fractions of the width.
fn apply_dropouts(buf: &mut [u8], w: usize, h: usize, drops: &[[f32; 4]; 8]) {
    for d in drops {
        if d[3] == 0.0 { continue; }
        let row = ((d[0] - 525.0 * 0.085) / (525.0 * 0.915) * h as f32).round() as i32;
        let thick = ((h as f32 / 480.0).round() as i32).max(1);
        let (x0, x1) = ((d[1] * w as f32) as i32, ((d[1] + d[2]) * w as f32) as i32);
        let amt = d[3].abs().min(1.0);
        let target = if d[3] > 0.0 { 235.0 } else { 6.0 };
        for y in row..row + thick {
            if y < 0 || y >= h as i32 { continue; }
            for x in x0.max(0)..x1.min(w as i32) {
                let edge = ((x - x0).min(x1 - x) as f32 / 6.0).clamp(0.0, 1.0);
                let a = amt * edge;
                let o = (y as usize * w + x as usize) * 4;
                let n = ((x * 7 + y * 13) % 11) as f32 - 5.0;
                for c in 0..3 { buf[o + c] = (buf[o + c] as f32 * (1.0 - a) + (target + n) * a).clamp(0.0, 255.0) as u8; }
            }
        }
    }
}

/// While the sync circuit recaptures after a roll, the top of the picture "flags" sideways.
fn apply_flagging(buf: &mut [u8], w: usize, h: usize, flag: f32, time: f32) {
    if flag < 0.01 { return; }
    let rows = (h as f32 * 0.12) as usize;
    let mut tmp = vec![0u8; w * 4];
    for y in 0..rows {
        let top = 1.0 - y as f32 / rows as f32;
        let shift = (flag * top * top * (0.06 * (y as f32 * 0.8 + time * 25.0).sin() + 0.03 * ((y * 7919) % 13) as f32 / 13.0) * w as f32) as i32;
        if shift == 0 { continue; }
        let row = &mut buf[y * w * 4..(y + 1) * w * 4];
        tmp.copy_from_slice(row);
        for x in 0..w as i32 {
            let sx = (x - shift).clamp(0, w as i32 - 1) as usize;
            row[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&tmp[sx * 4..sx * 4 + 4]);
        }
    }
}

fn smooth(a: f32, b: f32, x: f32) -> f32 { let t = ((x - a) / (b - a)).clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) }

/// Field rows for a band, from the ntsc-rs "Height" setting scaled to the processing resolution.
fn band_rows(e: &NtscEffect, mul: f32, frame_h: usize) -> i32 {
    (e.tracking_noise.settings.height as f32 * mul * (frame_h as f32 / 480.0)).round().max(1.0) as i32
}

/// Tracking-noise bars for the current tracking error (`signed` = knob - optimum). They are ntsc-rs's own tracking
/// noise, so the ntsc-rs "Tracking noise" settings (enable, height, wave, snow, anisotropy, noise) control them.
/// Layout "Bottom": one bar at the bottom whose height and strength grow with the error.
/// Layout "Middle": a bar appears mid-screen, a second splits off, and they slide toward the top and bottom.
pub fn tracking_bars(e: &NtscEffect, signed: f32, time: f32, frame_h: usize) -> Vec<TrackingBar> {
    if !e.tracking_noise.enabled { return Vec::new(); }
    let s = signed.abs().min(1.0);
    match e.tracking_noise.settings.layout {
        TrackingLayout::Bottom => vec![TrackingBar {
            end: 1.0, height: band_rows(e, 0.6 + 1.8 * s, frame_h), gain: 0.5 + 1.5 * smooth(0.0, 0.6, s),
        }],
        TrackingLayout::Middle => {
            let on = smooth(0.04, 0.5, s);
            if on < 0.01 { return Vec::new(); }
            let dir = if signed < 0.0 { -1.0 } else { 1.0 };
            let spread = (s * 0.9).min(0.45);
            let wob = 0.012 * (time * 0.9).sin();
            let gain = 0.35 + 1.65 * on;
            let m = 0.8 + 2.0 * s;
            let mut v = vec![TrackingBar { end: 0.55 + dir * spread * 0.9 + wob, height: band_rows(e, m, frame_h), gain }];
            if s > 0.15 { v.push(TrackingBar { end: 0.5 - dir * spread + wob, height: band_rows(e, m * 0.9, frame_h), gain: gain * 0.9 }); }
            if s > 0.6 { v.push(TrackingBar { end: 0.5 + dir * spread * 0.35 - wob, height: band_rows(e, m * 0.7, frame_h), gain: gain * 0.8 }); }
            v
        }
    }
}

/// Settings for the picture-search look (fast forward / rewind) and the pause bar. They are independent of the
/// ntsc-rs tracking-noise settings and of the tracking control.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchSettings {
    /// number of bars on screen
    pub bars: i32,
    /// height of each bar, percent of the picture
    pub width_pct: f32,
    /// how fast the bars roll, screens per second
    pub speed: f32,
    /// speckle snow inside the bars
    pub snow: f32,
    pub snow_anisotropy: f32,
    /// non-speckle noise inside the bars
    pub noise: f32,
    /// how much the lines inside a bar wave sideways
    pub wave: f32,
    /// thin snowy lines striking through the picture (0 = none)
    pub strikes: f32,
    /// how far the picture drops while searching, percent of its height
    pub drop_pct: f32,
    /// how much of the tape's normal noise stays during search (1 = all of it)
    pub resting_noise: f32,
}

impl Default for SearchSettings {
    fn default() -> Self {
        SearchSettings { bars: 5, width_pct: 7.0, speed: 0.0, snow: 1.0, snow_anisotropy: 0.15, noise: 0.6, wave: 36.0, strikes: 0.0, drop_pct: 4.0, resting_noise: 0.25 }
    }
}

/// Bars for picture search (`dir` = +1 fast forward, -1 rewind): they roll upward on fast forward and downward on
/// rewind. `dir` = 0 (pause) gives a single bar whose position follows the tracking error `track` (knob - optimum).
/// While the picture is dropped, the strip that opens at the top gets its own bar, so it fills with tracking snow.
pub fn search_bars(s: &SearchSettings, dir: f32, time: f32, frame_h: usize, track: f32) -> Vec<TrackingBar> {
    let rows = ((s.width_pct / 100.0 * frame_h as f32) * 0.5).round().max(1.0) as i32;
    let mut v = Vec::new();
    if s.drop_pct > 0.0 {
        v.push(TrackingBar { end: s.drop_pct / 100.0, height: ((s.drop_pct / 100.0 * frame_h as f32) * 0.5).round().max(1.0) as i32 + 2, gain: 1.0 });
    }
    if dir == 0.0 {
        let e = track.clamp(-1.0, 1.0);
        let wob = 0.01 * (time * 0.7).sin();
        let h = (rows as f32 * (1.0 + e.abs())).round() as i32;
        v.push(TrackingBar { end: (0.9 - 0.8 * e + wob).clamp(0.0, 1.1), height: h, gain: 1.0 });
        return v;
    }
    let n = s.bars.max(0) as usize;
    v.extend((0..n).map(|i| {
        let ph = (-dir * time * s.speed + i as f32 / n as f32).rem_euclid(1.0);
        TrackingBar { end: ph * 1.1, height: rows, gain: 1.0 }
    }));
    v
}

/// The effect for search/pause: the bars use these settings instead of the tracking-noise ones.
/// `rest` scales the tape's normal noise (1 keeps it).
pub fn search_effect(base: &NtscEffect, s: &SearchSettings, bars: Vec<TrackingBar>, rest: f32) -> NtscEffect {
    let mut e = with_bars(base, bars);
    let t = &mut e.tracking_noise.settings;
    t.wave_intensity = s.wave;
    t.snow_intensity = s.snow;
    t.snow_anisotropy = s.snow_anisotropy;
    t.noise_intensity = s.noise;
    e.composite_noise.settings.intensity *= rest;
    e.luma_noise.settings.intensity *= rest;
    e.snow_intensity *= rest;
    e.chroma_noise.settings.intensity *= 0.5 + 0.5 * rest;
    e
}

/// The effect with the given bars. The built-in bottom tracking noise is replaced by the bars (same code, same settings).
pub fn with_bars(base: &NtscEffect, bars: Vec<TrackingBar>) -> NtscEffect {
    let mut e = base.clone();
    e.tracking_bars = bars;
    e.tracking_noise.enabled = false;
    e
}

fn hash(a: u32, b: u32) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15; x = x.wrapping_mul(0x2C1B_3C6D); x ^= x >> 12; x = x.wrapping_mul(0x297A_2D39); x ^= x >> 15;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Picture search and pause drop the picture slightly because the head is no longer locked to the vertical sync.
/// The strip that opens at the top is mid-grey here; the tracking bar over it (see `search_bars`) fills it with snow.
fn apply_vshift(buf: &mut [u8], w: usize, h: usize, frac: f32) {
    let sh = (frac * h as f32) as usize;
    if sh == 0 || sh >= h { return; }
    buf.copy_within(0..(h - sh) * w * 4, sh * w * 4);
    for px in buf[..sh * w * 4].chunks_exact_mut(4) { px[0] = 70; px[1] = 70; px[2] = 70; }
}

/// Picture search: thin snowy lines strike straight through the picture, 
/// They change every frame, like the head crossing tracks it is not locked to.
fn apply_strikes(buf: &mut [u8], w: usize, h: usize, frame: u32, k: f32) {
    if k <= 0.0 { return; }
    let scale = (h as f32 / 480.0).max(1.0);
    let noise_px = |buf: &mut [u8], y: usize, x0: usize, x1: usize, a: f32, seed: u32| {
        let run = 2 + (hash(seed, 5) * 3.0) as usize;
        let mut n = 0.0f32;
        for x in x0..x1.min(w) {
            if (x - x0) % run == 0 { n = hash(x as u32 * 17 + seed, y as u32 + seed * 3); }
            let v = (n * n.sqrt() * 300.0 + 20.0).min(255.0);
            let o = (y * w + x) * 4;
            for c in 0..3 { buf[o + c] = (buf[o + c] as f32 * (1.0 - a) + v * a) as u8; }
        }
    };
    // thin strikes
    let strikes = (3.0 + 4.0 * k) as u32;
    for i in 0..strikes {
        let y0 = (hash(i * 13 + 1, frame) * h as f32) as usize;
        let th = ((1.0 + hash(i, frame ^ 0x55) * 2.5) * scale) as usize;
        let x0 = if hash(i + 50, frame) < 0.55 { 0 } else { (hash(i + 60, frame) * 0.5 * w as f32) as usize };
        let x1 = if hash(i + 70, frame) < 0.55 { w } else { w - (hash(i + 80, frame) * 0.4 * w as f32) as usize };
        let a = 0.6 + 0.4 * hash(i + 90, frame);
        for y in y0..(y0 + th).min(h) { noise_px(buf, y, x0, x1, a, i * 7 + y as u32); }
    }
}

pub struct Job {
    /// Picture stretch (x, y) about the centre, applied before the VHS effect; >1 zooms in and crops.
    pub scale: [f32; 2],
    pub strikes: f32,
    pub v_shift: f32,
    pub src: Arc<FrameData>,
    pub effect: NtscEffect,
    pub frame_num: usize,
    pub drops: [[f32; 4]; 8],
    pub flag: f32,
    pub time: f32,
    pub bypass: bool,
}

/// Stretch the picture about its centre (bilinear). Areas outside the source become black.
fn apply_scale(src: &[u8], w: usize, h: usize, sx: f32, sy: f32) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    for y in 0..h {
        let fy = (y as f32 + 0.5 - cy) / sy + cy - 0.5;
        let row = &mut out[y * w * 4..(y + 1) * w * 4];
        if fy < -0.5 || fy > h as f32 - 0.5 { for px in row.chunks_exact_mut(4) { px[3] = 255; } continue; }
        let fy = fy.clamp(0.0, h as f32 - 1.0);
        let (y0, ty) = (fy as usize, fy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..w {
            let fx = (x as f32 + 0.5 - cx) / sx + cx - 0.5;
            let o = x * 4;
            if fx < -0.5 || fx > w as f32 - 0.5 { row[o + 3] = 255; continue; }
            let fx = fx.clamp(0.0, w as f32 - 1.0);
            let (x0, tx) = (fx as usize, fx.fract());
            let x1 = (x0 + 1).min(w - 1);
            for c in 0..3 {
                let s = |yy: usize, xx: usize| src[(yy * w + xx) * 4 + c] as f32;
                let a = s(y0, x0) * (1.0 - tx) + s(y0, x1) * tx;
                let b = s(y1, x0) * (1.0 - tx) + s(y1, x1) * tx;
                row[o + c] = (a * (1.0 - ty) + b * ty + 0.5) as u8;
            }
            row[o + 3] = 255;
        }
    }
    out
}

pub struct Out {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// Run the full picture path on one frame (used by the worker thread and by `--render-test`).
pub fn process(job: &Job) -> Out {
    let (w, h) = (job.src.w as usize, job.src.h as usize);
    let mut buf = if (job.scale[0] - 1.0).abs() > 1e-3 || (job.scale[1] - 1.0).abs() > 1e-3 {
        apply_scale(&job.src.rgba, w, h, job.scale[0].max(0.1), job.scale[1].max(0.1))
    } else { job.src.rgba.clone() };
    if !job.bypass {
        apply_vshift(&mut buf, w, h, job.v_shift);
        job.effect.apply_effect_to_buffer::<Rgbx, u8>(ntsc_rs::ctx::global(), (w, h), &mut buf, job.frame_num, [1.0, 1.0]);
        apply_strikes(&mut buf, w, h, job.frame_num as u32, job.strikes);
        apply_dropouts(&mut buf, w, h, &job.drops);
        apply_flagging(&mut buf, w, h, job.flag, job.time);
    }
    for px in buf.chunks_exact_mut(4) { px[3] = 255; }
    Out { w, h, rgba: buf }
}

/// Background worker so a slow effect never stalls the UI or audio.
pub struct Processor {
    tx: Sender<Job>,
    pub rx: Receiver<Out>,
    busy: Arc<AtomicBool>,
}

impl Processor {
    pub fn new() -> Self {
        let (tx, jobs) = channel::<Job>();
        let (done_tx, rx) = channel::<Out>();
        let busy = Arc::new(AtomicBool::new(false));
        let b = busy.clone();
        std::thread::spawn(move || {
            while let Ok(job) = jobs.recv() {
                let out = process(&job);
                let _ = done_tx.send(out);
                b.store(false, Ordering::Release);
            }
        });
        Processor { tx, rx, busy }
    }
    pub fn idle(&self) -> bool { !self.busy.load(Ordering::Acquire) }
    pub fn submit(&self, job: Job) {
        self.busy.store(true, Ordering::Release);
        let _ = self.tx.send(job);
    }
}
