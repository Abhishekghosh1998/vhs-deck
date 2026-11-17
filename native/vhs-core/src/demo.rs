//! Built-in demo tape: a procedural picture (no fonts needed) and a small synthesised tune.
use crate::rng::Rng;
use std::f32::consts::TAU;

fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] { [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)] }

/// Render an RGBA frame of `w`x`h` for tape time `t` seconds.
pub fn demo_frame(t: f32, w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 255);
    let horizon = 480.0 * 0.62;
    let bars: [[f32; 3]; 8] = [
        [255., 255., 255.], [255., 255., 0.], [0., 255., 255.], [0., 255., 0.],
        [255., 0., 255.], [255., 0., 0.], [0., 0., 255.], [0., 0., 0.],
    ];
    let (bx, by) = (280.0 + (t * 2.1).sin() * 200.0, 400.0 - (t * 3.3).sin().abs() * 70.0);
    let sun = (520.0, 86.0 + (t * 0.3).sin() * 8.0);
    for y in 0..h {
        let dy = (y as f32 + 0.5) * 480.0 / h as f32;
        for x in 0..w {
            let dx = (x as f32 + 0.5) * 640.0 / w as f32;
            let mut c: [f32; 3];
            if dy < horizon {
                c = mix3([47., 111., 208.], [196., 230., 255.], dy / horizon);
                if (dx - sun.0).powi(2) + (dy - sun.1).powi(2) < 36.0 * 36.0 { c = [255., 210., 63.]; }
                for i in 0..4 {
                    let cx = ((t * (10.0 + 6.0 * i as f32) + 190.0 * i as f32) % 860.0) - 110.0;
                    let cy = 50.0 + 38.0 * i as f32;
                    if ((dx - cx) / 56.0).powi(2) + ((dy - cy) / 16.0).powi(2) < 1.0 { c = [240., 245., 255.]; }
                }
                if ((dx - 150.0) / 280.0).powi(2) + ((dy - horizon) / 80.0).powi(2) < 1.0 { c = [46., 158., 79.]; }
                if ((dx - 470.0) / 260.0).powi(2) + ((dy - horizon) / 60.0).powi(2) < 1.0 { c = [31., 122., 58.]; }
                // title card: thick red frame with "text" strokes
                let (fx0, fx1, fy0, fy1) = (150.0, 490.0, 190.0, 262.0);
                if dx > fx0 && dx < fx1 && dy > fy0 && dy < fy1 {
                    let edge = dx < fx0 + 6.0 || dx > fx1 - 6.0 || dy < fy0 + 6.0 || dy > fy1 - 6.0;
                    if edge { c = [230., 40., 30.]; }
                    else {
                        c = [20., 20., 30.];
                        let row = ((dy - fy0 - 14.0) / 14.0).floor();
                        if row >= 0.0 && row < 3.0 && (dy - fy0 - 14.0) % 14.0 < 7.0 && dx > fx0 + 20.0 && dx < fx0 + 300.0 - row * 60.0 { c = [255., 255., 255.]; }
                    }
                }
            } else if dy < horizon + 56.0 {
                c = bars[((dx / 80.0) as usize).min(7)];
            } else if dy < horizon + 78.0 {
                let v = (dx / 40.0).floor() / 15.0 * 255.0;
                c = [v, v, v];
            } else {
                c = [106., 106., 106.];
                // resolution wedge: lines getting finer
                if dx > 20.0 && dx < 240.0 && dy > horizon + 92.0 && dy < horizon + 152.0 {
                    c = [255., 255., 255.];
                    let i = ((dx - 20.0) / 5.4).floor();
                    let width = (3.2 - i * 0.075).max(0.6);
                    if (dx - 20.0) - i * 5.4 < width { c = [0., 0., 0.]; }
                }
            }
            if (dx - bx).abs() < 14.0 && (dy - by).abs() < 14.0 {
                c = if (dx - bx).abs() < 8.0 && (dy - by).abs() < 8.0 { [230., 30., 30.] } else { [255., 255., 255.] };
            }
            let o = (y * w + x) * 4;
            out[o] = c[0] as u8; out[o + 1] = c[1] as u8; out[o + 2] = c[2] as u8; out[o + 3] = 255;
        }
    }
}

/// Tiny arpeggio + bass + drums, 104 BPM, Am-F-C-G.
pub struct DemoSynth {
    sr: f32,
    n: u64,
    last_step: i64,
    rng: Rng,
    bass: (f32, f32, f32), // (freq, age, phase)
    arp: (f32, f32, f32),
    kick: (f32, f32),      // (age, phase)
    hat: f32,
    snare: f32,
    lp: f32,
    prev_noise: f32,
}

fn mtof(m: f32) -> f32 { 440.0 * 2f32.powf((m - 69.0) / 12.0) }

impl DemoSynth {
    pub fn new(sr: f32) -> Self {
        DemoSynth { sr, n: 0, last_step: -1, rng: Rng::new(42), bass: (0.0, 9.0, 0.0), arp: (0.0, 9.0, 0.0),
            kick: (9.0, 0.0), hat: 9.0, snare: 9.0, lp: 0.0, prev_noise: 0.0 }
    }
    pub fn seek_seconds(&mut self, t: f32) { self.n = (t.max(0.0) * self.sr) as u64; self.last_step = -1; }

    /// Fill interleaved stereo.
    pub fn render(&mut self, out: &mut [f32]) {
        let step_len = self.sr * 60.0 / 104.0 / 4.0;
        let dt = 1.0 / self.sr;
        for frame in out.chunks_mut(2) {
            let step = (self.n as f32 / step_len) as i64;
            if step != self.last_step {
                self.last_step = step;
                let bar = ((step / 16) % 4) as usize;
                let s = (step % 16) as usize;
                let r = [45.0, 41.0, 48.0, 43.0][bar];
                let third = if bar == 0 { 3.0 } else { 4.0 };
                if s % 4 == 0 {
                    self.bass = (mtof(r - 12.0 + if s == 8 { 7.0 } else { 0.0 }), 0.0, 0.0);
                    self.kick = (0.0, 0.0);
                }
                if s % 2 == 0 {
                    let tones = [r + 12.0, r + 12.0 + third, r + 19.0, r + 24.0];
                    let idx = [0, 1, 2, 3, 2, 1, 2, 1][(s >> 1) % 8];
                    self.arp = (mtof(tones[idx]), 0.0, 0.0);
                    self.hat = 0.0;
                }
                if s % 8 == 4 { self.snare = 0.0; }
            }
            let mut v = 0.0;
            // bass: saw through a one-pole low-pass
            self.bass.2 = (self.bass.2 + self.bass.0 * dt) % 1.0;
            self.bass.1 += dt;
            self.lp += ((self.bass.2 * 2.0 - 1.0) * (-self.bass.1 * 7.0).exp() - self.lp) * 0.05;
            v += self.lp * 0.5;
            // arp: triangle
            self.arp.2 = (self.arp.2 + self.arp.0 * dt) % 1.0;
            self.arp.1 += dt;
            v += ((self.arp.2 * 2.0 - 1.0).abs() * 2.0 - 1.0) * (-self.arp.1 * 10.0).exp() * 0.16;
            // kick
            self.kick.0 += dt;
            self.kick.1 = (self.kick.1 + (40.0 + 110.0 * (-self.kick.0 * 30.0).exp()) * dt) % 1.0;
            v += (self.kick.1 * TAU).sin() * (-self.kick.0 * 18.0).exp() * 0.5;
            // hat + snare: differentiated noise
            let nz = self.rng.bipolar();
            let hp = nz - self.prev_noise;
            self.prev_noise = nz;
            self.hat += dt;
            self.snare += dt;
            v += hp * (-self.hat * 60.0).exp() * 0.05 + hp * (-self.snare * 20.0).exp() * 0.12;
            frame[0] = v;
            if frame.len() > 1 { frame[1] = v; }
            self.n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_frame_fills_every_pixel_including_the_bottom_rows() {
        let mut buf = Vec::new();
        demo_frame(1.0, 640, 480, &mut buf);
        assert_eq!(buf.len(), 640 * 480 * 4);
        // the lower ground area is flat grey (106) away from the wedge and the bouncing square
        for y in [400usize, 440, 470, 479] {
            let o = (y * 640 + 500) * 4;
            assert_eq!(&buf[o..o + 4], &[106, 106, 106, 255], "row {y}");
        }
    }
}
