//! Audio output (cpal). The audio callback is the master clock: the tape position only advances
//! when samples are actually consumed, so video follows audio and pausing freezes both.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use vhs_core::audio::{AudioParams, TapeAudio};
use vhs_core::demo::DemoSynth;
use vhs_core::params::{Knobs, Speed};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct Ring {
    pub gen: u64,
    pub q: VecDeque<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source { None, File, Demo }

pub struct AState {
    pub source: Source,
    pub playing: bool,
    pub has_file_audio: bool,
    pub clock_base: f64,
    pub frames: u64,
    pub params: AudioParams,
    pub dropouts: Vec<f32>,
    tape: TapeAudio,
    demo: DemoSynth,
}

pub struct AudioOut {
    _stream: cpal::Stream,
    pub ring: Arc<Mutex<Ring>>,
    pub state: Arc<Mutex<AState>>,
    pub sample_rate: u32,
}

impl AudioOut {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let cfg = device.default_output_config().map_err(|e| e.to_string())?;
        let sr = cfg.sample_rate().0;
        let ring = Arc::new(Mutex::new(Ring { gen: 0, q: VecDeque::new() }));
        let state = Arc::new(Mutex::new(AState {
            source: Source::None, playing: false, has_file_audio: false, clock_base: 0.0, frames: 0,
            params: AudioParams { knobs: Knobs::preset(vhs_core::params::Cond::Good), speed: Speed::Sp, track_err: 0.0, playing: false, volume: 0.8 },
            dropouts: Vec::new(),
            tape: TapeAudio::new(sr as f32),
            demo: DemoSynth::new(sr as f32),
        }));
        let sc = cfg.config();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &sc, ring.clone(), state.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &sc, ring.clone(), state.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &sc, ring.clone(), state.clone()),
            f => Err(format!("unsupported sample format {f:?}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(AudioOut { _stream: stream, ring, state, sample_rate: sr })
    }

    /// Tape position in seconds according to samples played.
    pub fn clock(&self) -> f64 {
        let s = self.state.lock().unwrap();
        s.clock_base + s.frames as f64 / self.sample_rate as f64
    }

    /// Jump to `pos`; returns the new ring generation (decoders must tag their samples with it).
    pub fn seek(&self, pos: f64) -> u64 {
        let gen = {
            let mut r = self.ring.lock().unwrap();
            r.gen += 1;
            r.q.clear();
            r.gen
        };
        let mut s = self.state.lock().unwrap();
        s.clock_base = pos;
        s.frames = 0;
        s.demo.seek_seconds(pos as f32);
        gen
    }

    pub fn set_source(&self, source: Source, has_file_audio: bool) {
        let mut s = self.state.lock().unwrap();
        s.source = source;
        s.has_file_audio = has_file_audio;
        if source == Source::None { s.playing = false; }
    }
    pub fn set_playing(&self, playing: bool) { self.state.lock().unwrap().playing = playing; }
    pub fn set_params(&self, p: AudioParams) { self.state.lock().unwrap().params = p; }
    pub fn dropout(&self, dur: f32) { self.state.lock().unwrap().dropouts.push(dur); }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    ring: Arc<Mutex<Ring>>,
    state: Arc<Mutex<AState>>,
) -> Result<cpal::Stream, String> {
    let ch = cfg.channels as usize;
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            cfg,
            move |data: &mut [T], _| {
                let frames = data.len() / ch.max(1);
                scratch.clear();
                scratch.resize(frames * 2, 0.0);
                {
                    let mut guard = state.lock().unwrap();
                    let st = &mut *guard;
                    if st.playing {
                        match st.source {
                            Source::Demo => {
                                st.demo.render(&mut scratch);
                                st.frames += frames as u64;
                            }
                            Source::File => {
                                let mut r = ring.lock().unwrap();
                                let n = r.q.len().min(scratch.len()) & !1;
                                for (i, s) in r.q.drain(..n).enumerate() { scratch[i] = s; }
                                drop(r);
                                // no audio track: the clock free-runs; otherwise it only advances with real samples
                                st.frames += if st.has_file_audio { (n / 2) as u64 } else { frames as u64 };
                            }
                            Source::None => {}
                        }
                    }
                    let drops: Vec<f32> = st.dropouts.drain(..).collect();
                    for d in drops { st.tape.dropout(d); }
                    let mut p = st.params;
                    p.playing = st.playing && st.source != Source::None;
                    st.tape.process(&mut scratch, &p);
                }
                for (i, frame) in data.chunks_mut(ch.max(1)).enumerate() {
                    for (c, out) in frame.iter_mut().enumerate() {
                        *out = if c < 2 { T::from_sample(scratch[i * 2 + c]) } else { T::from_sample(0.0) };
                    }
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}
