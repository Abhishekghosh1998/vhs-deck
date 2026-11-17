#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed { Sp, Lp, Ep }

#[derive(Clone, Copy, Debug)]
pub struct SpeedSpec {
    pub name: &'static str,
    /// Luma / chroma gaussian sigma in output pixels (of a 640-wide raster).
    pub luma: f32,
    pub chroma: f32,
    pub snr: f32,
    pub jit: f32,
    pub bw: f32,
    pub hiss: f32,
    pub wow: f32,
}

const SPEEDS: [SpeedSpec; 3] = [
    SpeedSpec { name: "SP", luma: 0.85, chroma: 4.0, snr: 1.0,  jit: 0.35, bw: 1.0,  hiss: 1.0, wow: 1.0 },
    SpeedSpec { name: "LP", luma: 1.30, chroma: 5.5, snr: 1.35, jit: 0.60, bw: 0.72, hiss: 1.3, wow: 1.4 },
    SpeedSpec { name: "EP", luma: 1.80, chroma: 7.5, snr: 1.8,  jit: 0.90, bw: 0.42, hiss: 1.7, wow: 2.0 },
];

impl Speed {
    pub const ALL: [Speed; 3] = [Speed::Sp, Speed::Lp, Speed::Ep];
    pub fn idx(self) -> usize { self as usize }
    pub fn from_idx(i: usize) -> Speed { Speed::ALL[i.min(2)] }
    pub fn spec(self) -> &'static SpeedSpec { &SPEEDS[self.idx()] }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cond { Good, Worn, Damaged }

#[derive(Clone, Copy, Debug)]
pub struct CondSpec {
    pub name: &'static str,
    /// dropouts per second, rolls per second, small vertical bounces per second
    pub drop: f32,
    pub roll: f32,
    pub bounce: f32,
    /// how far the optimum tracking position wanders
    pub trk: f32,
    pub jit: f32,
}

const CONDS: [CondSpec; 3] = [
    CondSpec { name: "GOOD",    drop: 0.15, roll: 0.002, bounce: 0.02, trk: 0.06, jit: 0.6 },
    CondSpec { name: "WORN",    drop: 2.0,  roll: 0.02,  bounce: 0.15, trk: 0.22, jit: 1.3 },
    CondSpec { name: "DAMAGED", drop: 7.0,  roll: 0.08,  bounce: 0.5,  trk: 0.45, jit: 2.6 },
];

impl Cond {
    pub const ALL: [Cond; 3] = [Cond::Good, Cond::Worn, Cond::Damaged];
    pub fn idx(self) -> usize { self as usize }
    pub fn from_idx(i: usize) -> Cond { Cond::ALL[i.min(2)] }
    pub fn spec(self) -> &'static CondSpec { &CONDS[self.idx()] }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Knobs {
    pub noise: f32,
    pub head: f32,
    pub color: f32,
    pub hiss: f32,
    pub wow: f32,
    pub bw: f32,
}

impl Knobs {
    /// Choosing a tape condition loads its knob preset (the user can then tweak).
    pub fn preset(c: Cond) -> Knobs {
        match c {
            Cond::Good    => Knobs { noise: 0.10, head: 0.15, color: 0.10, hiss: 0.10, wow: 0.10, bw: 0.85 },
            Cond::Worn    => Knobs { noise: 0.35, head: 0.45, color: 0.40, hiss: 0.35, wow: 0.35, bw: 0.55 },
            Cond::Damaged => Knobs { noise: 0.70, head: 0.80, color: 0.75, hiss: 0.65, wow: 0.70, bw: 0.30 },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode { None, Stop, Play, Pause, Ff, Rew }

/// Audio low-pass cutoff (Hz) for the AUDIO BANDWIDTH knob at the given tape speed.
pub fn audio_cutoff(bw: f32, speed: Speed) -> f32 {
    (1200.0 * 12f32.powf(bw) * speed.spec().bw).clamp(300.0, 19000.0)
}
