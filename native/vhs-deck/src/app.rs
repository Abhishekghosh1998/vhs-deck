//! The player: transport logic and a VLC-style window (menu bar, video area, seek bar and controls)
//! with the VCR controls and every ntsc-rs setting in a side panel.
use crate::audio_out::{AudioOut, Source};
use crate::media::{self, grab_range, DecodeSession, FrameData, MediaInfo, VFrame};
use crate::process::{self, Job, Processor};
use eframe::egui::{self, Align2, Color32, FontId, Mesh, Painter, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureId, TextureOptions, Ui, Vec2};
use ntsc_rs::settings::{AnySetting, SettingDescriptor, SettingKind, Settings};
use ntsc_rs::NtscEffect;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;
use vhs_core::audio::AudioParams;
use vhs_core::demo::demo_frame;
use vhs_core::params::{Cond, Knobs, Mode, Speed};
use vhs_core::sim::{Sim, SimCtx};

const ORANGE: Color32 = Color32::from_rgb(255, 136, 0);

struct FileTape {
    info: MediaInfo,
    dec: Option<DecodeSession>,
    pending: Option<VFrame>,
}
enum Tape { None, File(FileTape), Demo }

#[derive(Clone, Copy, PartialEq)]
enum Zoom { Fit, Fill, Stretch, Scale(f32) }

const ASPECTS: [(&str, Option<f32>); 9] = [
    ("Default", None), ("16:9", Some(16.0 / 9.0)), ("4:3", Some(4.0 / 3.0)), ("1:1", Some(1.0)),
    ("16:10", Some(1.6)), ("2.21:1", Some(2.21)), ("2.35:1", Some(2.35)), ("2.39:1", Some(2.39)), ("5:4", Some(1.25)),
];
const ZOOMS: [(&str, Zoom); 7] = [
    ("Fit to window", Zoom::Fit), ("Fill window (crop)", Zoom::Fill), ("Stretch to window", Zoom::Stretch),
    ("1:4 Quarter", Zoom::Scale(0.25)), ("1:2 Half", Zoom::Scale(0.5)), ("1:1 Original", Zoom::Scale(1.0)), ("2:1 Double", Zoom::Scale(2.0)),
];
const PROC: [(&str, u32); 6] = [("360 lines", 360), ("480 lines (NTSC)", 480), ("576 lines (PAL)", 576), ("720 lines", 720), ("1080 lines", 1080), ("Source resolution", 0)];

#[derive(Clone, Copy, PartialEq)]
enum Pick { Tape, Url, LoadPreset, SavePreset }
#[derive(Clone, Copy, PartialEq)]
enum Tab { Vcr, Search, Advanced }

pub struct App {
    effect: NtscEffect,
    descriptors: Vec<SettingDescriptor<NtscEffect>>,
    cond: Cond,
    knobs: Knobs,
    track: f32,
    auto: bool,
    roll_on: bool,
    bypass: bool,
    search: process::SearchSettings,
    mode: Mode,
    volume: f32,
    muted: bool,
    sim: Sim,
    view: vhs_core::sim::View,
    tape: Tape,
    audio: Option<AudioOut>,
    started: Instant,
    last: Instant,
    wall_clock: f64,
    // picture
    proc: Processor,
    src_frame: Option<Arc<FrameData>>,
    src_dirty: bool,
    last_submit: Instant,
    frame_num: usize,
    tex: Option<TextureHandle>,
    tex_size: (usize, usize),
    src_dims: (u32, u32),
    // view options
    aspect: usize,
    zoom: Zoom,
    smooth: bool,
    proc_h: u32,
    rate: f32,
    rate_ui: Option<f32>,
    scale: [f32; 2],
    scale_link: bool,
    proc_edit: u32,
    show_panel: bool,
    tab: Tab,
    fullscreen: bool,
    ui_until: f64,
    last_mouse: Pos2,
    osd_msg: Option<(String, f64)>,
    scrub: Option<f64>,
    search_pos: f64,
    search_acc: f32,
    search_cache: Vec<(f64, Arc<FrameData>)>,
    search_req: Option<(Receiver<Vec<(f64, FrameData)>>, f64, f64)>,
    search_lo: f64,
    search_hi: f64,
    search_shown: f64,
    picker: Option<(Pick, Receiver<Option<PathBuf>>)>,
    url_rx: Option<Receiver<Result<MediaInfo, String>>>,
    demo_step: i64,
    demo_buf: Vec<u8>,
    ended: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        let mut v = egui::Visuals::dark();
        v.selection.bg_fill = ORANGE;
        v.selection.stroke = Stroke::new(1.0, Color32::BLACK);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ORANGE);
        cc.egui_ctx.set_visuals(v);
        let (audio, _audio_err) = match AudioOut::new() { Ok(a) => (Some(a), None), Err(e) => (None, Some(e)) };
        let now = Instant::now();
        let cond = Cond::Good;
        let mut app = App {
            effect: process::preset(cond), descriptors: NtscEffect::setting_descriptors().into_vec(), cond,
            knobs: Knobs::preset(cond), track: 0.0, auto: true, roll_on: true, bypass: false, search: Default::default(), mode: Mode::None,
            volume: 0.8, muted: false, sim: Sim::new(now.elapsed().as_nanos() as u64 ^ 0x5eed), view: Default::default(),
            tape: Tape::None, audio, started: now, last: now, wall_clock: 0.0,
            proc: Processor::new(), src_frame: None, src_dirty: false, last_submit: now, frame_num: 0, tex: None,
            tex_size: (4, 3), src_dims: (640, 480),
            aspect: 0, zoom: Zoom::Fit, smooth: true, proc_h: 480, rate: 1.0, rate_ui: None, scale: [1.0, 1.0], scale_link: true, proc_edit: 480, show_panel: true, tab: Tab::Vcr, fullscreen: false,
            ui_until: 0.0, last_mouse: Pos2::ZERO, osd_msg: None, scrub: None,
            search_pos: 0.0, search_acc: 0.0, search_cache: Vec::new(), search_req: None, search_lo: 0.0, search_hi: 0.0, search_shown: -1.0, picker: None, url_rx: None,
            demo_step: -1, demo_buf: Vec::new(), ended: false,
        };
        if let Some(p) = initial {
            let s = p.to_string_lossy().to_string();
            if media::is_url(&s) { app.open_url(&s); } else { app.load(p); }
        }
        app
    }

    // ---------------------------------------------------------------- transport
    fn secs(&self) -> f64 { self.started.elapsed().as_secs_f64() }
    fn msg(&mut self, m: &str, secs: f64) { self.osd_msg = Some((m.to_string(), self.secs() + secs)); }
    fn clock(&self) -> f64 { self.audio.as_ref().map(|a| a.clock()).unwrap_or(self.wall_clock) }
    fn pos(&self) -> f64 { if matches!(self.mode, Mode::Ff | Mode::Rew) { self.search_pos } else { self.clock() } }
    fn duration(&self) -> Option<f64> { if let Tape::File(f) = &self.tape { Some(f.info.duration) } else { None } }
    fn has_tape(&self) -> bool { !matches!(self.tape, Tape::None) }

    fn eject(&mut self) {
        self.tape = Tape::None;
        self.mode = Mode::None;
        self.src_frame = None;
        self.tex = None;
        if let Some(a) = &self.audio { a.set_source(Source::None, false); }
    }

    fn set_frame(&mut self, f: FrameData) {
        self.src_dims = match &self.tape { Tape::File(ft) => (ft.info.src_w, ft.info.src_h), _ => (f.w, f.h) };
        self.src_frame = Some(Arc::new(f));
        self.src_dirty = true;
    }

    fn insert_demo(&mut self) {
        self.eject();
        self.tape = Tape::Demo;
        self.demo_step = -1;
        if let Some(a) = &self.audio { a.set_source(Source::Demo, false); }
        self.seek(0.0);
        self.play();
    }

    fn load(&mut self, path: PathBuf) {
        match media::probe(&path, self.proc_h) {
            Err(e) => self.msg(&format!("Cannot open file: {e}"), 4.0),
            Ok(info) => self.load_info(info),
        }
    }

    fn open_url(&mut self, url: &str) {
        if self.url_rx.is_some() { return; }
        self.msg("Loading web video… (needs yt-dlp)", 60.0);
        let (url, max_h) = (url.trim().to_string(), if self.proc_h == 0 { 1080 } else { self.proc_h.max(480) });
        let (tx, rx) = channel();
        self.url_rx = Some(rx);
        std::thread::spawn(move || { let _ = tx.send(media::resolve_url(&url, max_h)); });
    }

    fn load_info(&mut self, info: MediaInfo) {
        {
            {
                self.eject();
                if let Some(a) = &self.audio { a.set_source(Source::File, info.has_audio); }
                let title = info.title.clone();
                self.tape = Tape::File(FileTape { info, dec: None, pending: None });
                self.mode = Mode::Stop;
                self.seek(0.0);
                self.play();
                self.msg(&title, 3.0);
            }
        }
    }

    /// Restart decoding from `pos` (also resets the audio clock).
    fn seek(&mut self, pos: f64) {
        let pos = pos.max(0.0);
        self.wall_clock = pos;
        self.ended = false;
        let gen = self.audio.as_ref().map(|a| a.seek(pos)).unwrap_or(0);
        let ring = self.audio.as_ref().map(|a| (a.ring.clone(), gen, a.sample_rate));
        let mut err = None;
        if let Tape::File(ft) = &mut self.tape {
            ft.dec = None; // kills the old ffmpeg processes first
            ft.pending = None;
            match DecodeSession::start(&ft.info, pos, ring, self.rate as f64) {
                Ok(d) => ft.dec = Some(d),
                Err(e) => err = Some(e),
            }
        }
        if let Some(e) = err { self.msg(&format!("Decode error: {e}"), 4.0); }
    }

    /// Playback speed (0.25x to 4x, pitch preserved), like VLC's rate control.
    fn set_rate(&mut self, r: f32) {
        let r = r.clamp(0.25, 4.0);
        if (r - self.rate).abs() < 1e-3 { return; }
        let pos = self.pos();
        self.rate = r;
        if let Some(a) = &self.audio { a.set_rate(r as f64); }
        if self.has_tape() && !matches!(self.mode, Mode::Ff | Mode::Rew | Mode::Stop) { self.seek(pos); }
        self.msg(&format!("Speed {:.2}x", r), 1.5);
    }

    fn leave_search(&mut self) {
        if matches!(self.mode, Mode::Ff | Mode::Rew) {
            let p = self.search_pos;
            self.seek(p);
        }
    }

    fn play(&mut self) {
        if !self.has_tape() { self.msg("Open a file first (Ctrl+O)", 3.0); return; }
        if matches!(self.mode, Mode::Stop | Mode::Ff | Mode::Rew) {
            let p = self.pos();
            self.seek(p);
        }
        self.mode = Mode::Play;
        if let Some(a) = &self.audio { a.set_playing(true); }
    }
    fn pause(&mut self) {
        if !self.has_tape() { return; }
        if self.mode == Mode::Pause { self.play(); return; }
        self.leave_search();
        self.mode = Mode::Pause;
        if let Some(a) = &self.audio { a.set_playing(false); }
    }
    fn toggle_play(&mut self) { if self.mode == Mode::Play { self.pause() } else { self.play() } }
    fn stop(&mut self) {
        if !self.has_tape() { return; }
        self.leave_search();
        self.mode = Mode::Stop;
        if let Some(a) = &self.audio { a.set_playing(false); }
    }
    fn search(&mut self, m: Mode) {
        if !self.has_tape() { return; }
        if self.mode == m { let p = self.search_pos; self.mode = Mode::Pause; self.seek(p); self.play(); return; }
        if !matches!(self.mode, Mode::Ff | Mode::Rew) { self.search_pos = self.pos(); }
        if let Tape::File(ft) = &mut self.tape { ft.dec = None; ft.pending = None; }
        self.mode = m;
        self.search_cache.clear();
        self.search_req = None;
        self.search_lo = self.search_pos;
        self.search_hi = self.search_pos;
        self.search_shown = -1.0;
        if let Some(a) = &self.audio { a.set_playing(false); }
    }
    fn skip(&mut self, d: f64) {
        if matches!(self.mode, Mode::Play | Mode::Pause) && self.has_tape() {
            let mut p = (self.clock() + d).max(0.0);
            if let Some(dur) = self.duration() { if dur > 0.0 { p = p.min(dur - 0.5); } }
            self.seek(p.max(0.0));
        }
    }
    fn adjust_track(&mut self, d: f32) {
        self.track = (self.track + d).clamp(-1.0, 1.0);
        self.auto = false;
    }

    fn set_proc_height(&mut self, h: u32) {
        self.proc_h = h;
        let p = self.pos();
        let live = matches!(self.mode, Mode::Play | Mode::Pause);
        if let Tape::File(ft) = &mut self.tape { ft.info.set_proc_height(h); }
        if live { self.seek(p); }
    }

    fn pick(&mut self, kind: Pick) {
        if self.picker.is_some() { return; }
        let (tx, rx) = channel();
        self.picker = Some((kind, rx));
        std::thread::spawn(move || {
            let mut c = std::process::Command::new("zenity");
            if kind != Pick::Url { c.arg("--file-selection"); }
            match kind {
                Pick::Url => { c.args(["--entry", "--title=Open URL", "--text=Paste a YouTube (or other web video) link:", "--width=520"]); }
                Pick::Tape => { c.arg("--title=Open media"); }
                Pick::LoadPreset => { c.args(["--title=Load ntsc-rs preset", "--file-filter=ntsc-rs presets | *.json"]); }
                Pick::SavePreset => { c.args(["--title=Save ntsc-rs preset", "--save", "--confirm-overwrite", "--filename=vhs-preset.json"]); }
            }
            let out = c.output().ok();
            let path = out.filter(|o| o.status.success()).map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()));
            let _ = tx.send(path);
        });
    }

    fn finish_pick(&mut self, kind: Pick, path: PathBuf) {
        match kind {
            Pick::Tape => if path.is_file() { self.load(path) },
            Pick::Url => { let u = path.to_string_lossy().to_string(); if media::is_url(&u) { self.open_url(&u) } else { self.msg("That does not look like a web link", 3.0) } }
            Pick::LoadPreset => {
                let list = ntsc_rs::settings::SettingsList::<NtscEffect>::new();
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| list.from_json(&s).map_err(|e| e.to_string())) {
                    Ok(e) => { self.effect = e; self.msg("Preset loaded", 2.0); }
                    Err(e) => self.msg(&format!("Preset error: {e}"), 4.0),
                }
            }
            Pick::SavePreset => {
                let list = ntsc_rs::settings::SettingsList::<NtscEffect>::new();
                let r = list.to_json_string(&self.effect).map_err(|e| format!("{e:?}")).and_then(|s| std::fs::write(&path, s).map_err(|e| e.to_string()));
                match r { Ok(()) => self.msg("Preset saved", 2.0), Err(e) => self.msg(&format!("Save failed: {e}"), 4.0) }
            }
        }
    }

    // ---------------------------------------------------------------- per-frame work
    fn advance(&mut self, dt: f32) {
        if self.audio.is_none() && self.mode == Mode::Play { self.wall_clock += dt as f64 * self.rate as f64; }
        let secs = self.secs();
        if let Some((_, until)) = &self.osd_msg { if secs > *until { self.osd_msg = None; } }
        if let Some((kind, rx)) = &self.picker {
            let kind = *kind;
            match rx.try_recv() {
                Ok(p) => { self.picker = None; if let Some(p) = p { self.finish_pick(kind, p); } }
                Err(TryRecvError::Disconnected) => self.picker = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        if let Some(rx) = &self.url_rx {
            match rx.try_recv() {
                Ok(r) => { self.url_rx = None; self.osd_msg = None; match r { Ok(info) => self.load_info(info), Err(e) => self.msg(&format!("Web video: {e}"), 8.0) } }
                Err(TryRecvError::Disconnected) => self.url_rx = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        // FF / REW: position runs at 6x and a still frame is grabbed ~7 times a second
        if matches!(self.mode, Mode::Ff | Mode::Rew) {
            let dir = if self.mode == Mode::Ff { 1.0 } else { -1.0 };
            self.search_pos += dir * 6.0 * dt as f64;
            let dur = self.duration().unwrap_or(f64::MAX);
            if self.search_pos <= 0.0 { self.search_pos = 0.0; self.stop(); }
            else if self.search_pos >= dur { self.search_pos = (dur - 0.5).max(0.0); self.stop(); }
            self.advance_search(dir);
        }

        let clock = self.clock();
        let mode = self.mode;
        let proc_h = if self.proc_h == 0 { 480 } else { self.proc_h } as usize;
        let mut newest: Option<FrameData> = None;
        let mut ended = false;
        match &mut self.tape {
            Tape::File(ft) if matches!(mode, Mode::Play | Mode::Pause) => {
                if let Some(dec) = &ft.dec {
                    loop {
                        if ft.pending.is_none() {
                            match dec.rx.try_recv() {
                                Ok(f) => ft.pending = Some(f),
                                Err(TryRecvError::Disconnected) => { ended = true; break; }
                                Err(TryRecvError::Empty) => break,
                            }
                        }
                        match ft.pending.take() {
                            Some(f) if f.pts <= clock + 0.001 => newest = Some(f.data),
                            Some(f) => { ft.pending = Some(f); break; }
                            None => break,
                        }
                    }
                }
            }
            Tape::Demo => {
                let t = if matches!(mode, Mode::Ff | Mode::Rew) { self.search_pos } else { clock };
                let step = (t * 30.0).floor() as i64;
                if step != self.demo_step && !matches!(mode, Mode::None | Mode::Stop) {
                    self.demo_step = step;
                    let w = proc_h * 4 / 3 & !1;
                    demo_frame(t as f32, w, proc_h, &mut self.demo_buf);
                    newest = Some(FrameData { w: w as u32, h: proc_h as u32, rgba: self.demo_buf.clone() });
                }
            }
            _ => {}
        }
        if let Some(f) = newest { self.set_frame(f); }
        if ended && mode == Mode::Play && !self.ended {
            self.ended = true;
            self.stop();
            self.seek(0.0);
        }

        // simulation (picture roll, dropouts, tracking drift) + audio parameters
        self.knobs.noise = process::get_noise(&self.effect);
        self.knobs.head = process::get_head(&self.effect);
        self.knobs.color = process::get_colour(&self.effect);
        let speed = process::speed_of(&self.effect);
        let ctx = SimCtx { mode: self.mode, cond: self.cond, speed, knobs: self.knobs };
        let drops = self.sim.update(dt, &ctx, &mut self.track, self.auto);
        self.view = self.sim.view(dt, &ctx);
        if !self.roll_on { self.view.roll = 0.0; self.view.flag = 0.0; }
        if let Some(a) = &self.audio {
            for d in drops { a.dropout(d); }
            a.set_params(AudioParams {
                knobs: self.knobs, speed, track_err: self.sim.track_error(),
                playing: self.mode == Mode::Play, volume: if self.muted { 0.0 } else { self.volume },
            });
        }
    }

    /// Picture search plays real frames at about 6x: short chunks are decoded ahead of (FF) or behind (REW)
    /// the search position, and the frame nearest to it is shown.
    fn advance_search(&mut self, dir: f64) {
        const STEP: f64 = 0.25;
        const CHUNK: f64 = 4.0;
        let sp = self.search_pos;
        if let Some((rx, from, to)) = &self.search_req {
            if let Ok(v) = rx.try_recv() {
                let (from, to) = (*from, *to);
                self.search_req = None;
                self.search_lo = self.search_lo.min(from);
                self.search_hi = self.search_hi.max(to);
                for (p, f) in v { self.search_cache.push((p, Arc::new(f))); }
                self.search_cache.sort_by(|a, b| a.0.total_cmp(&b.0));
                self.search_cache.retain(|(p, _)| (p - sp).abs() < 20.0);
            }
        }
        if self.search_req.is_none() {
            if let Tape::File(ft) = &self.tape {
                let need = if dir > 0.0 { self.search_hi < sp + 2.5 } else { self.search_lo > sp - 2.5 && self.search_lo > 0.0 };
                if need {
                    let dur = ft.info.duration;
                    let (from, to) = if dir > 0.0 { (self.search_hi, (self.search_hi + CHUNK).min(dur)) } else { ((self.search_lo - CHUNK).max(0.0), self.search_lo) };
                    if to > from {
                        let (info, (tx, rx)) = (ft.info.clone(), channel());
                        std::thread::spawn(move || { let _ = tx.send(grab_range(&info, from, to - from, STEP)); });
                        self.search_req = Some((rx, from, to));
                    }
                }
            }
        }
        let best = self.search_cache.iter().min_by(|a, b| (a.0 - sp).abs().total_cmp(&(b.0 - sp).abs()));
        if let Some((p, f)) = best {
            if (p - sp).abs() < 1.0 && *p != self.search_shown {
                self.search_shown = *p;
                self.src_dims = match &self.tape { Tape::File(ft) => (ft.info.src_w, ft.info.src_h), _ => (f.w, f.h) };
                self.src_frame = Some(f.clone());
                self.src_dirty = true;
            }
        }
    }

    /// Collect finished frames and start the next one on the worker.
    fn pump_picture(&mut self, ctx: &egui::Context) {
        let opts = if self.smooth { TextureOptions::LINEAR } else { TextureOptions::NEAREST };
        while let Ok(o) = self.proc.rx.try_recv() {
            let img = egui::ColorImage::from_rgba_unmultiplied([o.w, o.h], &o.rgba);
            self.tex_size = (o.w, o.h);
            match &mut self.tex {
                Some(t) => t.set(img, opts),
                None => self.tex = Some(ctx.load_texture("picture", img, opts)),
            }
        }
        let Some(src) = self.src_frame.clone() else { return };
        if !matches!(self.mode, Mode::Play | Mode::Pause | Mode::Ff | Mode::Rew) { return; }
        // a paused or searching VCR keeps re-reading the same field, so the noise keeps moving
        let refresh = self.mode != Mode::Play && self.last_submit.elapsed().as_secs_f32() > 1.0 / 30.0;
        if !(self.src_dirty || refresh) || !self.proc.idle() { return; }
        self.src_dirty = false;
        self.last_submit = Instant::now();
        self.frame_num = self.frame_num.wrapping_add(1);
        let time = self.secs() as f32;
        let fh = src.h as usize;
        let ss = &self.search;
        let (v_shift, strikes, effect) = match self.mode {
            Mode::Ff => (ss.drop_pct / 100.0, ss.strikes, process::search_effect(&self.effect, ss, process::search_bars(ss, 1.0, time, fh, 0.0), ss.resting_noise)),
            Mode::Rew => (ss.drop_pct / 100.0, ss.strikes, process::search_effect(&self.effect, ss, process::search_bars(ss, -1.0, time, fh, 0.0), ss.resting_noise)),
            Mode::Pause => (ss.drop_pct / 100.0, 0.0, process::search_effect(&self.effect, ss, process::search_bars(ss, 0.0, time, fh, self.sim.track_signed()), 1.0)),
            _ => (0.0, 0.0, process::with_bars(&self.effect, process::tracking_bars(&self.effect, self.sim.track_signed(), time, fh))),
        };
        self.proc.submit(Job {
            scale: self.scale,
            v_shift, strikes, src, effect, frame_num: self.frame_num,
            drops: self.view.drops, flag: self.view.flag, time: self.secs() as f32, bypass: self.bypass,
        });
    }

    // ---------------------------------------------------------------- input
    fn handle_input(&mut self, ctx: &egui::Context) {
        for f in ctx.input(|i| i.raw.dropped_files.clone()) {
            if let Some(p) = f.path { self.load(p); }
        }
        for t in ctx.input(|i| i.events.iter().filter_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None }).collect::<Vec<_>>()) {
            if media::is_url(&t) { self.open_url(&t); }
        }
        use egui::Key;
        let (faster, slower) = ctx.input(|i| (i.key_pressed(Key::Equals) || i.key_pressed(Key::Plus), i.key_pressed(Key::Minus)));
        if faster { let r = next_speed(self.rate, true); self.set_rate(r); }
        if slower { let r = next_speed(self.rate, false); self.set_rate(r); }
        let (space, left, right, up, down, m, f, esc, ctrl_o, lb, rb) = ctx.input(|i| (
            i.key_pressed(Key::Space), i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight), i.key_pressed(Key::ArrowUp),
            i.key_pressed(Key::ArrowDown), i.key_pressed(Key::M), i.key_pressed(Key::F), i.key_pressed(Key::Escape),
            i.modifiers.command && i.key_pressed(Key::O), i.key_pressed(Key::OpenBracket), i.key_pressed(Key::CloseBracket),
        ));
        if space { self.toggle_play(); }
        if left { self.skip(-5.0); }
        if right { self.skip(5.0); }
        if up { self.volume = (self.volume + 0.05).min(1.0); self.msg(&format!("Volume {:.0}%", self.volume * 100.0), 1.5); }
        if down { self.volume = (self.volume - 0.05).max(0.0); self.msg(&format!("Volume {:.0}%", self.volume * 100.0), 1.5); }
        if m { self.muted = !self.muted; self.msg(if self.muted { "Muted" } else { "Unmuted" }, 1.5); }
        if f { self.set_fullscreen(ctx, !self.fullscreen); }
        if esc && self.fullscreen { self.set_fullscreen(ctx, false); }
        if ctrl_o { self.pick(Pick::Tape); }
        if lb { self.adjust_track(-0.05); self.msg(&format!("Tracking {:+.2}", self.track), 1.5); }
        if rb { self.adjust_track(0.05); self.msg(&format!("Tracking {:+.2}", self.track), 1.5); }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    // ---------------------------------------------------------------- UI pieces
    fn menu_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("Media", |ui| {
                if ui.button("Open File…        Ctrl+O").clicked() { self.pick(Pick::Tape); ui.close_menu(); }
                if ui.button("Open URL… (YouTube)   Ctrl+V").clicked() { self.pick(Pick::Url); ui.close_menu(); }
                if ui.button("Play built-in demo tape").clicked() { self.insert_demo(); ui.close_menu(); }
                if ui.button("Close").clicked() { self.eject(); ui.close_menu(); }
                ui.separator();
                if ui.button("Quit").clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
            });
            ui.menu_button("Playback", |ui| {
                if ui.button("Play / Pause     Space").clicked() { self.toggle_play(); ui.close_menu(); }
                if ui.button("Stop").clicked() { self.stop(); ui.close_menu(); }
                ui.separator();
                ui.menu_button("Speed", |ui| {
                    for r in SPEEDS {
                        if ui.radio((self.rate - r).abs() < 1e-3, format!("{r}x")).clicked() { self.set_rate(r); ui.close_menu(); }
                    }
                });
                if ui.button("Faster     =").clicked() { let r = next_speed(self.rate, true); self.set_rate(r); }
                if ui.button("Slower     -").clicked() { let r = next_speed(self.rate, false); self.set_rate(r); }
                if ui.button("Normal speed").clicked() { self.set_rate(1.0); ui.close_menu(); }
                ui.separator();
                if ui.button("Jump back 5 s     ←").clicked() { self.skip(-5.0); ui.close_menu(); }
                if ui.button("Jump forward 5 s  →").clicked() { self.skip(5.0); ui.close_menu(); }
                if ui.button("Jump back 30 s").clicked() { self.skip(-30.0); ui.close_menu(); }
                if ui.button("Jump forward 30 s").clicked() { self.skip(30.0); ui.close_menu(); }
                ui.separator();
                if ui.button("Rewind (picture search)").clicked() { self.search(Mode::Rew); ui.close_menu(); }
                if ui.button("Fast forward (picture search)").clicked() { self.search(Mode::Ff); ui.close_menu(); }
            });
            ui.menu_button("Audio", |ui| {
                if ui.checkbox(&mut self.muted, "Mute          M").clicked() { ui.close_menu(); }
                if ui.button("Volume up       ↑").clicked() { self.volume = (self.volume + 0.1).min(1.0); }
                if ui.button("Volume down     ↓").clicked() { self.volume = (self.volume - 0.1).max(0.0); }
            });
            ui.menu_button("Video", |ui| {
                if ui.checkbox(&mut self.fullscreen.clone(), "Fullscreen    F").clicked() { self.set_fullscreen(ctx, !self.fullscreen); ui.close_menu(); }
                ui.separator();
                ui.menu_button("Aspect Ratio", |ui| {
                    for (i, (name, _)) in ASPECTS.iter().enumerate() {
                        if ui.radio(self.aspect == i, *name).clicked() { self.aspect = i; ui.close_menu(); }
                    }
                });
                ui.menu_button("Scale / Zoom", |ui| {
                    for (name, z) in ZOOMS {
                        if ui.radio(self.zoom == z, name).clicked() { self.zoom = z; ui.close_menu(); }
                    }
                });
                ui.menu_button("Processing resolution", |ui| {
                    for (name, h) in PROC {
                        if ui.radio(self.proc_h == h, name).clicked() { self.proc_edit = h; self.set_proc_height(h); ui.close_menu(); }
                    }
                });
                ui.menu_button("Scaling filter", |ui| {
                    if ui.radio(self.smooth, "Smooth (bilinear)").clicked() { self.smooth = true; ui.close_menu(); }
                    if ui.radio(!self.smooth, "Sharp (nearest)").clicked() { self.smooth = false; ui.close_menu(); }
                });
            });
            ui.menu_button("VCR", |ui| {
                ui.checkbox(&mut self.bypass, "Bypass VCR effects (compare with the clean picture)");
                ui.checkbox(&mut self.roll_on, "Allow picture roll");
                if ui.button("Roll the picture now").clicked() { self.sim.force_roll(); ui.close_menu(); }
                ui.separator();
                if ui.button("Load ntsc-rs preset…").clicked() { self.pick(Pick::LoadPreset); ui.close_menu(); }
                if ui.button("Save ntsc-rs preset…").clicked() { self.pick(Pick::SavePreset); ui.close_menu(); }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.show_panel, "VCR / effects panel");
            });
        });
    }

    fn controls(&mut self, ui: &mut Ui) {
        let dur = self.duration().unwrap_or(0.0);
        let pos = self.pos().clamp(0.0, dur.max(0.0));
        ui.horizontal(|ui| {
            ui.monospace(fmt_time(self.scrub.unwrap_or(pos)));
            let mut p = self.scrub.unwrap_or(pos);
            ui.spacing_mut().slider_width = (ui.available_width() - 80.0).max(50.0);
            let r = ui.add_enabled(dur > 0.0, egui::Slider::new(&mut p, 0.0..=dur.max(0.001)).show_value(false));
            if r.dragged() { self.scrub = Some(p); }
            if r.drag_stopped() || (r.changed() && !r.dragged()) { self.scrub = None; self.seek_ui(p); }
            ui.monospace(fmt_time(dur));
        });
        ui.horizontal(|ui| {
            let playing = self.mode == Mode::Play;
            if icon_button(ui, Icon::Rew, self.mode == Mode::Rew).on_hover_text("Rewind (picture search)").clicked() { self.search(Mode::Rew); }
            if icon_button(ui, if playing { Icon::Pause } else { Icon::Play }, false).on_hover_text("Play / Pause (Space)").clicked() { self.toggle_play(); }
            if icon_button(ui, Icon::Stop, false).on_hover_text("Stop").clicked() { self.stop(); }
            if icon_button(ui, Icon::Ff, self.mode == Mode::Ff).on_hover_text("Fast forward (picture search)").clicked() { self.search(Mode::Ff); }
            ui.separator();
            let mode_txt = match self.mode { Mode::None => "NO MEDIA", Mode::Stop => "STOP", Mode::Play => "PLAY", Mode::Pause => "PAUSE", Mode::Ff => "FF", Mode::Rew => "REW" };
            ui.label(egui::RichText::new(mode_txt).small().color(Color32::GRAY));
            ui.separator();
            ui.label(egui::RichText::new("Speed").small().color(Color32::GRAY));
            let mut r = self.rate_ui.unwrap_or(self.rate);
            ui.spacing_mut().slider_width = 90.0;
            let sr = ui.add(egui::Slider::new(&mut r, 0.25..=4.0).logarithmic(true).step_by(0.05).fixed_decimals(2).suffix("x"))
                .on_hover_text("Playback speed (- and =). Applied when you let go.");
            if sr.dragged() { self.rate_ui = Some(r); }
            else if sr.changed() { self.rate_ui = None; self.set_rate(r); }           // typed value or click
            if sr.drag_stopped() { self.rate_ui = None; self.set_rate(r); }
            if ui.small_button("1x").on_hover_text("Normal speed").clicked() { self.rate_ui = None; self.set_rate(1.0); }
            ui.separator();
            ui.label(egui::RichText::new("Tracking").small().color(Color32::GRAY));
            if ui.small_button("−").clicked() { self.adjust_track(-0.05); }
            ui.spacing_mut().slider_width = 130.0;
            let mut t = self.track;
            let r = ui.add(egui::Slider::new(&mut t, -1.0..=1.0).show_value(true).fixed_decimals(2)).on_hover_text("Tracking ([ and ])");
            if r.changed() && (r.dragged() || r.has_focus() || r.clicked()) { self.track = t; self.auto = false; }
            if ui.small_button("+").clicked() { self.adjust_track(0.05); }
            ui.label(egui::RichText::new("Auto").small().color(Color32::GRAY));
            toggle_switch(ui, &mut self.auto).on_hover_text("Auto tracking: follow the tape's drifting best tracking point");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().slider_width = 110.0;
                let mut v = self.volume;
                if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() { self.volume = v; self.muted = false; }
                if icon_button(ui, if self.muted { Icon::Mute } else { Icon::Speaker }, false).on_hover_text("Mute (M)").clicked() { self.muted = !self.muted; }
                ui.label(egui::RichText::new(format!("{:.0}%", if self.muted { 0.0 } else { self.volume * 100.0 })).small().color(Color32::GRAY));
            });
        });
    }

    fn seek_ui(&mut self, p: f64) {
        if !self.has_tape() { return; }
        if matches!(self.mode, Mode::Stop | Mode::Ff | Mode::Rew) { self.mode = Mode::Pause; }
        self.seek(p);
        if self.mode == Mode::None { self.mode = Mode::Pause; }
    }

    fn side_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Vcr, "VCR");
            ui.selectable_value(&mut self.tab, Tab::Search, "FF / REW");
            ui.selectable_value(&mut self.tab, Tab::Advanced, "All ntsc-rs settings");
        });
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            match self.tab {
                Tab::Vcr => self.vcr_tab(ui),
                Tab::Search => self.search_tab(ui),
                Tab::Advanced => self.advanced_tab(ui),
            }
        });
    }

    fn vcr_tab(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        ui.strong("Resolution");
        ui.horizontal_wrapped(|ui| {
            for (name, h) in [("480", 480u32), ("576", 576), ("720", 720), ("1080", 1080), ("Source", 0)] {
                if ui.selectable_label(self.proc_h == h, name).clicked() { self.proc_edit = h; self.set_proc_height(h); }
            }
            let r = ui.add(egui::DragValue::new(&mut self.proc_edit).range(120..=2160).speed(4).suffix(" lines"));
            if (r.drag_stopped() || r.lost_focus()) && self.proc_edit != self.proc_h && self.proc_edit >= 120 { let h = self.proc_edit; self.set_proc_height(h); }
        }).response.on_hover_text("The video is scaled to this many lines before the VHS effect, as with the scale option in ntsc-rs. Use 1080 on a 1080p video for an HD VHS look. Higher costs more CPU.");

        ui.add_space(8.0);
        ui.strong("Picture scale");
        ui.horizontal(|ui| {
            let mut ch = false;
            let old = self.scale;
            ch |= ui.add(egui::DragValue::new(&mut self.scale[0]).range(0.25..=4.0).speed(0.005).prefix("X ").fixed_decimals(3)).changed();
            let lbl = if self.scale_link { "🔗 linked" } else { "unlinked" };
            if ui.toggle_value(&mut self.scale_link, lbl).on_hover_text("Linked: X and Y change together").clicked() && self.scale_link {
                self.scale[1] = self.scale[0]; ch = true;
            }
            ch |= ui.add(egui::DragValue::new(&mut self.scale[1]).range(0.25..=4.0).speed(0.005).prefix("Y ").fixed_decimals(3)).changed();
            if ch && self.scale_link {
                if self.scale[0] != old[0] { self.scale[1] = self.scale[0]; } else { self.scale[0] = self.scale[1]; }
            }
            if ui.button("Reset").clicked() { self.scale = [1.0, 1.0]; ch = true; }
            if ch { self.src_dirty = true; }
        }).response.on_hover_text("Stretch the picture before the VHS effect. For example X=1, Y=1.45 (unlinked) crops away letterbox bars.");

        ui.add_space(8.0);
        ui.strong("Tape");
        ui.horizontal(|ui| {
            ui.label("Condition");
            for c in Cond::ALL {
                if ui.selectable_label(self.cond == c, c.spec().name).clicked() {
                    let speed = process::speed_of(&self.effect);
                    self.cond = c;
                    self.effect = process::preset(c);
                    process::set_speed(&mut self.effect, speed);
                    self.knobs = Knobs::preset(c);
                }
            }
        }).response.on_hover_text("Loads a starting point for every setting. You can then adjust anything.");
        ui.horizontal(|ui| {
            ui.label("Speed");
            let cur = process::speed_of(&self.effect);
            for s in Speed::ALL {
                if ui.selectable_label(cur == s, s.spec().name).clicked() { process::set_speed(&mut self.effect, s); }
            }
        }).response.on_hover_text("SP is sharpest. LP and EP lose luma bandwidth and smear colour more.");

        ui.add_space(8.0);
        ui.strong("Tracking");
        ui.horizontal(|ui| {
            if ui.small_button(" − ").clicked() { self.adjust_track(-0.05); }
            ui.spacing_mut().slider_width = (ui.available_width() - 120.0).max(60.0);
            let mut t = self.track;
            let r = ui.add(egui::Slider::new(&mut t, -1.0..=1.0).show_value(true).fixed_decimals(2));
            if r.changed() && (r.dragged() || r.has_focus() || r.clicked()) { self.track = t; self.auto = false; }
            if ui.small_button(" + ").clicked() { self.adjust_track(0.05); }
        });
        ui.horizontal(|ui| {
            toggle_switch(ui, &mut self.auto).on_hover_text("Auto tracking: follow the tape's drifting best tracking point");
            ui.label("Auto tracking");
            ui.label(egui::RichText::new(if self.auto { "on" } else { "off" }).small().color(Color32::GRAY));
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let e = self.sim.track_error();
            ui.label(egui::RichText::new("Tracking error").small());
            ui.label(egui::RichText::new(format!("{:.2}", e)).small().monospace().color(Color32::GRAY));
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(40.0), 10.0), Sense::hover());
            ui.painter().rect_filled(rect, 2.0, Color32::from_gray(40));
            let w = rect.width() * e.clamp(0.0, 1.0);
            let col = if e < 0.2 { Color32::from_rgb(90, 200, 120) } else if e < 0.5 { Color32::from_rgb(230, 190, 60) } else { Color32::from_rgb(230, 80, 60) };
            ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(w, rect.height())), 2.0, col);
        }).response.on_hover_text("Distance between the tracking slider and the tape's best tracking point. Keep it near zero for a clean picture; auto tracking does that for you.");

        ui.label(egui::RichText::new("How the bars look (height, wave, snow, noise, position) is set under All ntsc-rs settings → Tracking noise.").small().color(Color32::GRAY));

        ui.add_space(8.0);
        ui.strong("Picture");
        let mut noise = process::get_noise(&self.effect);
        if pct_slider(ui, "Video noise", &mut noise) { process::set_noise(&mut self.effect, noise); }
        let mut head = process::get_head(&self.effect);
        if pct_slider(ui, "Head switching", &mut head) { process::set_head(&mut self.effect, head); }
        let mut colour = process::get_colour(&self.effect);
        if pct_slider(ui, "Colour bleed / fade", &mut colour) { process::set_colour(&mut self.effect, colour); }
        ui.checkbox(&mut self.roll_on, "Picture roll");
        ui.horizontal(|ui| {
            if ui.button("Roll now").clicked() { self.sim.force_roll(); }
            ui.checkbox(&mut self.bypass, "Bypass effects");
        });

        ui.add_space(8.0);
        ui.strong("Audio");
        pct_slider(ui, "Audio hiss", &mut self.knobs.hiss);
        pct_slider(ui, "Wow & flutter", &mut self.knobs.wow);
        pct_slider(ui, "Audio bandwidth", &mut self.knobs.bw);
        ui.label(egui::RichText::new("Bandwidth low = muffled").small().color(Color32::GRAY));
    }

    fn search_tab(&mut self, ui: &mut Ui) {
        let s = &mut self.search;
        ui.add_space(4.0);
        ui.label(egui::RichText::new("The look of fast forward, rewind and pause. Separate from the tracking settings.").small().color(Color32::GRAY));
        ui.add_space(4.0);
        ui.add(egui::Slider::new(&mut s.bars, 0..=10).text("Number of bars"));
        ui.add(egui::Slider::new(&mut s.width_pct, 0.5..=40.0).text("Bar height %"));
        ui.add(egui::Slider::new(&mut s.speed, 0.0..=4.0).text("Roll speed"));
        ui.add(egui::Slider::new(&mut s.snow, 0.0001..=1.0).logarithmic(true).text("Snow intensity"));
        ui.add(egui::Slider::new(&mut s.snow_anisotropy, 0.0..=1.0).text("Snow anisotropy"));
        ui.add(egui::Slider::new(&mut s.noise, 0.0..=1.0).text("Noise intensity"));
        ui.add(egui::Slider::new(&mut s.wave, -50.0..=50.0).text("Wave intensity"));
        ui.add_space(6.0);
        ui.add(egui::Slider::new(&mut s.strikes, 0.0..=2.0).text("Snow lines through picture"));
        ui.add(egui::Slider::new(&mut s.drop_pct, 0.0..=15.0).text("Picture drop %"));
        ui.add(egui::Slider::new(&mut s.resting_noise, 0.0..=1.0).text("Normal tape noise kept"));
        ui.add_space(6.0);
        if ui.button("Reset to defaults").clicked() { *s = Default::default(); }
    }

    fn advanced_tab(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("Load preset…").clicked() { self.pick(Pick::LoadPreset); }
            if ui.button("Save preset…").clicked() { self.pick(Pick::SavePreset); }
            if ui.button("Reset").clicked() { self.effect = NtscEffect::default(); }
        });
        ui.label(egui::RichText::new("Presets use the ntsc-rs JSON format.").small().color(Color32::GRAY));
        ui.separator();
        let descs = std::mem::take(&mut self.descriptors);
        for d in &descs { setting_ui(ui, d, &mut self.effect); }
        self.descriptors = descs;
    }

    // ---------------------------------------------------------------- video area
    fn video(&self, ui: &mut Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        let live = matches!(self.mode, Mode::Play | Mode::Pause | Mode::Ff | Mode::Rew);
        match (&self.tex, live) {
            (Some(tex), true) => {
                let src_ar = self.tex_size.0 as f32 / self.tex_size.1 as f32;
                let ar = ASPECTS[self.aspect].1.unwrap_or(src_ar);
                let panel = rect.size();
                let fit = |a: f32| if panel.x / panel.y > a { Vec2::new(panel.y * a, panel.y) } else { Vec2::new(panel.x, panel.x / a) };
                let size = match self.zoom {
                    Zoom::Stretch => panel,
                    Zoom::Fit => fit(ar),
                    Zoom::Fill => { let f = fit(ar); let s = (panel.x / f.x).max(panel.y / f.y); f * s }
                    Zoom::Scale(z) => { let h = self.src_dims.1 as f32 * z / ui.ctx().pixels_per_point(); Vec2::new(h * ar, h) }
                };
                let r = Rect::from_center_size(rect.center(), size);
                paint_rolled(&painter, tex.id(), r, self.view.roll);
            }
            _ => {
                let text = if self.has_tape() { "" } else { "Open a file (Ctrl+O) or drop one on this window" };
                painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(18.0), Color32::from_gray(120));
            }
        }
        if let Some((m, _)) = &self.osd_msg {
            let pos = rect.left_top() + Vec2::new(16.0, 14.0);
            painter.text(pos + Vec2::splat(1.5), Align2::LEFT_TOP, m, FontId::proportional(20.0), Color32::BLACK);
            painter.text(pos, Align2::LEFT_TOP, m, FontId::proportional(20.0), Color32::WHITE);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        self.handle_input(ctx);
        self.advance(dt);
        self.pump_picture(ctx);

        // in fullscreen the bars appear while the mouse moves
        let mouse = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(self.last_mouse);
        if (mouse - self.last_mouse).length() > 2.0 { self.ui_until = self.secs() + 2.5; }
        self.last_mouse = mouse;
        let chrome = !self.fullscreen;
        let controls_visible = !self.fullscreen || self.secs() < self.ui_until;

        if chrome {
            egui::TopBottomPanel::top("menu").show(ctx, |ui| self.menu_bar(ui, ctx));
        }
        if controls_visible {
            egui::TopBottomPanel::bottom("controls").show(ctx, |ui| { ui.add_space(2.0); self.controls(ui); ui.add_space(2.0); });
        }
        if chrome && self.show_panel {
            egui::SidePanel::right("side").resizable(true).default_width(330.0).width_range(260.0..=520.0).show(ctx, |ui| self.side_panel(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::BLACK)).show(ctx, |ui| {
            let rect = ui.max_rect();
            let r = ui.allocate_rect(rect, Sense::click());
            if r.double_clicked() { self.set_fullscreen(ctx, !self.fullscreen); }
            else if r.clicked() { self.toggle_play(); }
            self.video(ui, rect);
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(if self.mode == Mode::Play { 4 } else { 16 }));
    }
}

// ---------------------------------------------------------------------------- helpers

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

fn pct_slider(ui: &mut Ui, label: &str, v: &mut f32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized([120.0, 18.0], egui::Label::new(label));
        ui.spacing_mut().slider_width = (ui.available_width() - 56.0).max(50.0);
        changed = ui.add(egui::Slider::new(v, 0.0..=1.0).custom_formatter(|x, _| format!("{:.0}%", x * 100.0)).custom_parser(|s| s.trim_end_matches('%').parse::<f64>().ok().map(|x| x / 100.0))).changed();
    });
    changed
}

/// Draw the picture, optionally rolled: the raster is the picture plus an 8.5% blanking interval, and the
/// window onto it slides, so the black bar with the sync pulse scrolls through the screen.
fn paint_rolled(painter: &Painter, tex: TextureId, r: Rect, roll: f32) {
    const VBI: f32 = 0.085;
    let white = Color32::WHITE;
    if roll.abs() < 1e-4 {
        painter.image(tex, r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), white);
        return;
    }
    let a = (VBI + roll).rem_euclid(1.0);
    let span = 1.0 - VBI;
    let y_of = |x: f32| r.top() + (x - a) / span * r.height();
    let mut mesh = Mesh::with_texture(tex);
    let mut blanks: Vec<(f32, f32)> = Vec::new();
    for k in -1..=1 {
        let k = k as f32;
        let (s, e) = (VBI + k, 1.0 + k);
        let (lo, hi) = (s.max(a), e.min(a + span));
        if hi > lo {
            let uv = Rect::from_x_y_ranges(0.0..=1.0, ((lo - s) / span)..=((hi - s) / span));
            mesh.add_rect_with_uv(Rect::from_x_y_ranges(r.x_range(), y_of(lo)..=y_of(hi)), uv, white);
        }
        let (s, e) = (k, VBI + k);
        let (lo, hi) = (s.max(a), e.min(a + span));
        if hi > lo { blanks.push((y_of(lo), y_of(hi))); }
    }
    painter.add(Shape::mesh(mesh));
    for (y0, y1) in blanks {
        let band = Rect::from_x_y_ranges(r.x_range(), y0..=y1);
        painter.rect_filled(band, 0.0, Color32::from_gray(6));
        let h = y1 - y0;
        painter.rect_filled(Rect::from_x_y_ranges(r.x_range(), (y0 + h * 0.55)..=(y0 + h * 0.62)), 0.0, Color32::from_gray(38));
    }
}

#[derive(Clone, Copy)]
enum Icon { Play, Pause, Stop, Rew, Ff, Speaker, Mute }

fn icon_button(ui: &mut Ui, icon: Icon, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 26.0), Sense::click());
    let hot = resp.hovered() || active;
    let col = if hot { ORANGE } else { Color32::from_gray(210) };
    let p = ui.painter();
    if hot { p.rect_filled(rect, 4.0, Color32::from_gray(45)); }
    let c = rect.center();
    let tri = |cx: f32, dir: f32, s: f32| {
        Shape::convex_polygon(vec![Pos2::new(cx - dir * s * 0.6, c.y - s), Pos2::new(cx - dir * s * 0.6, c.y + s), Pos2::new(cx + dir * s * 0.8, c.y)], col, Stroke::NONE)
    };
    match icon {
        Icon::Play => { p.add(tri(c.x, 1.0, 7.0)); }
        Icon::Pause => {
            p.rect_filled(Rect::from_center_size(c - Vec2::new(4.0, 0.0), Vec2::new(4.0, 14.0)), 1.0, col);
            p.rect_filled(Rect::from_center_size(c + Vec2::new(4.0, 0.0), Vec2::new(4.0, 14.0)), 1.0, col);
        }
        Icon::Stop => { p.rect_filled(Rect::from_center_size(c, Vec2::splat(12.0)), 1.0, col); }
        Icon::Ff => { p.add(tri(c.x - 5.0, 1.0, 6.0)); p.add(tri(c.x + 5.0, 1.0, 6.0)); }
        Icon::Rew => { p.add(tri(c.x - 5.0, -1.0, 6.0)); p.add(tri(c.x + 5.0, -1.0, 6.0)); }
        Icon::Speaker | Icon::Mute => {
            p.add(Shape::convex_polygon(vec![
                Pos2::new(c.x - 8.0, c.y - 3.0), Pos2::new(c.x - 3.0, c.y - 3.0), Pos2::new(c.x + 2.0, c.y - 8.0),
                Pos2::new(c.x + 2.0, c.y + 8.0), Pos2::new(c.x - 3.0, c.y + 3.0), Pos2::new(c.x - 8.0, c.y + 3.0),
            ], col, Stroke::NONE));
            if matches!(icon, Icon::Mute) {
                p.line_segment([Pos2::new(c.x + 5.0, c.y - 5.0), Pos2::new(c.x + 11.0, c.y + 5.0)], Stroke::new(2.0_f32, Color32::from_rgb(230, 80, 60)));
                p.line_segment([Pos2::new(c.x + 5.0, c.y + 5.0), Pos2::new(c.x + 11.0, c.y - 5.0)], Stroke::new(2.0_f32, Color32::from_rgb(230, 80, 60)));
            } else {
                p.line_segment([Pos2::new(c.x + 6.0, c.y - 4.0), Pos2::new(c.x + 6.0, c.y + 4.0)], Stroke::new(2.0_f32, col));
            }
        }
    }
    resp
}

/// One ntsc-rs setting (or group of settings), generated from ntsc-rs's own setting descriptors,
/// so this panel always has exactly the controls, ranges and descriptions that ntsc-rs defines.
fn setting_ui(ui: &mut Ui, d: &SettingDescriptor<NtscEffect>, e: &mut NtscEffect) {
    let cur = (d.id.get)(e);
    let mut new = cur;
    match (&d.kind, cur) {
        (SettingKind::Group { children }, AnySetting::Bool(mut on)) => {
            let id = ui.make_persistent_id(d.label);
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| {
                    let r = ui.checkbox(&mut on, d.label);
                    if let Some(t) = d.description { r.on_hover_text(t); }
                })
                .body(|ui| {
                    ui.add_enabled_ui(on, |ui| { for c in children { setting_ui(ui, c, e); } });
                });
            new = AnySetting::Bool(on);
        }
        (SettingKind::Enumeration { options }, AnySetting::Enum(mut v)) => {
            ui.horizontal(|ui| {
                let r = ui.label(d.label);
                if let Some(t) = d.description { r.on_hover_text(t); }
                let sel = options.iter().find(|o| o.index == v).map(|o| o.label).unwrap_or("?");
                egui::ComboBox::from_id_salt(d.label).selected_text(sel).show_ui(ui, |ui| {
                    for o in options {
                        let r = ui.selectable_value(&mut v, o.index, o.label);
                        if let Some(t) = o.description { r.on_hover_text(t); }
                    }
                });
            });
            new = AnySetting::Enum(v);
        }
        (SettingKind::Percentage { logarithmic }, AnySetting::Float(mut v)) => {
            let r = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).logarithmic(*logarithmic).text(d.label)
                .custom_formatter(|x, _| format!("{:.1}%", x * 100.0)).custom_parser(|s| s.trim_end_matches('%').parse::<f64>().ok().map(|x| x / 100.0)));
            if let Some(t) = d.description { r.on_hover_text(t); }
            new = AnySetting::Float(v);
        }
        (SettingKind::FloatRange { range, logarithmic }, AnySetting::Float(mut v)) => {
            let r = ui.add(egui::Slider::new(&mut v, range.clone()).logarithmic(*logarithmic).text(d.label));
            if let Some(t) = d.description { r.on_hover_text(t); }
            new = AnySetting::Float(v);
        }
        (SettingKind::IntRange { range }, AnySetting::Int(mut v)) => {
            let r = ui.add(egui::Slider::new(&mut v, range.clone()).text(d.label));
            if let Some(t) = d.description { r.on_hover_text(t); }
            new = AnySetting::Int(v);
        }
        (SettingKind::Boolean, AnySetting::Bool(mut v)) => {
            let r = ui.checkbox(&mut v, d.label);
            if let Some(t) = d.description { r.on_hover_text(t); }
            new = AnySetting::Bool(v);
        }
        _ => {}
    }
    if new != cur { let _ = (d.id.set)(e, new); }
}

/// A small on/off switch (pill with a sliding knob).
fn toggle_switch(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(34.0, 18.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() { *on = !*on; resp.mark_changed(); }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let off = Color32::from_gray(70);
    let onc = Color32::from_rgb(60, 150, 235);
    let bg = Color32::from_rgb(
        (off.r() as f32 + (onc.r() as f32 - off.r() as f32) * t) as u8,
        (off.g() as f32 + (onc.g() as f32 - off.g() as f32) * t) as u8,
        (off.b() as f32 + (onc.b() as f32 - off.b() as f32) * t) as u8);
    let r = rect.height() / 2.0;
    ui.painter().rect_filled(rect, r, bg);
    let cx = rect.left() + r + t * (rect.width() - 2.0 * r);
    ui.painter().circle_filled(egui::pos2(cx, rect.center().y), r - 2.5, Color32::WHITE);
    resp
}

const SPEEDS: [f32; 9] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];

fn next_speed(cur: f32, up: bool) -> f32 {
    if up { SPEEDS.iter().copied().find(|s| *s > cur + 1e-3).unwrap_or(4.0) }
    else { SPEEDS.iter().rev().copied().find(|s| *s < cur - 1e-3).unwrap_or(0.25) }
}
