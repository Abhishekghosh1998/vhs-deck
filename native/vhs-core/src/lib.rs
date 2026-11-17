//! Pure-Rust VHS model shared by the native player (and mirrored by the web prototype).
pub mod audio;
pub mod demo;
pub mod params;
pub mod rng;
pub mod sim;

pub fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
