//! Media access through the `ffmpeg`/`ffprobe` binaries (no system dev libraries needed).
//! Video is decoded to RGBA at the chosen processing height, audio to interleaved f32 stereo at the output device rate.
use crate::audio_out::Ring;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// A decoded RGBA frame.
pub struct FrameData {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub title: String,
    pub duration: f64,
    /// size of the source picture
    pub src_w: u32,
    pub src_h: u32,
    /// decoded (processing) size: the source scaled to the processing height, even dimensions
    pub w: u32,
    pub h: u32,
    pub fps: f64,
    pub has_audio: bool,
    /// separate audio stream (YouTube serves video and audio apart); None = audio is in `path`
    pub audio_url: Option<String>,
}

pub struct VFrame {
    pub pts: f64,
    pub data: FrameData,
}

fn parse_rate(s: &str) -> f64 {
    let mut it = s.split('/');
    let n: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let d: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1.0);
    if d > 0.0 { n / d } else { 0.0 }
}

/// Size to decode at: the source scaled down to `proc_h` lines (0 = keep the source size).
/// Can also scale up (like ntsc-rs's scale-to-height).
pub fn fit_size(w: u32, h: u32, proc_h: u32) -> (u32, u32) {
    let s = if proc_h == 0 { 1.0 } else { proc_h as f64 / h as f64 };
    let even = |v: f64| ((v.round() as u32).max(2)) & !1;
    (even(w as f64 * s), even(h as f64 * s))
}

impl MediaInfo {
    pub fn set_proc_height(&mut self, proc_h: u32) {
        let (w, h) = fit_size(self.src_w, self.src_h, proc_h);
        self.w = w;
        self.h = h;
    }
}

pub fn probe(path: &Path, proc_h: u32) -> Result<MediaInfo, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height,avg_frame_rate,r_frame_rate:format=duration", "-of", "json"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe not found: {e}"))?;
    if !out.status.success() {
        return Err("not a readable media file".into());
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let streams = j["streams"].as_array().cloned().unwrap_or_default();
    let v = streams.iter().find(|s| s["codec_type"] == "video").ok_or("no video stream")?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let (sw, sh) = (v["width"].as_u64().unwrap_or(0) as u32, v["height"].as_u64().unwrap_or(0) as u32);
    if sw == 0 || sh == 0 { return Err("video has no size".into()); }
    let mut fps = parse_rate(v["avg_frame_rate"].as_str().unwrap_or("0/1"));
    if fps < 1.0 || fps > 120.0 { fps = parse_rate(v["r_frame_rate"].as_str().unwrap_or("25/1")); }
    if fps < 1.0 || fps > 120.0 { fps = 25.0; }
    let duration = j["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    let (w, h) = fit_size(sw, sh, proc_h);
    let title = path.file_stem().map(|s| s.to_string_lossy().to_uppercase()).unwrap_or_default();
    Ok(MediaInfo { path: path.to_owned(), title, duration, src_w: sw, src_h: sh, w, h, fps, has_audio, audio_url: None })
}

fn ffmpeg_video(info: &MediaInfo, pos: f64) -> Command {
    let mut c = Command::new("ffmpeg");
    c.args(["-v", "error", "-nostdin"]);
    input_args(&mut c, &info.path.to_string_lossy(), pos);
    c
        .args(["-an", "-vf", &format!("fps={:.5},scale={}:{}:flags=lanczos,format=rgba", info.fps, info.w, info.h)])
        .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"]);
    c
}

/// One running decode of the tape starting at `pos`. Dropping it stops both ffmpeg processes.
pub struct DecodeSession {
    pub rx: Receiver<VFrame>,
    stop: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<Child>>>,
}

impl DecodeSession {
    pub fn start(info: &MediaInfo, pos: f64, ring: Option<(Arc<Mutex<Ring>>, u64, u32)>) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = sync_channel::<VFrame>(6);

        let mut vc = ffmpeg_video(info, pos);
        let mut vchild = vc.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn()
            .map_err(|e| format!("ffmpeg not found: {e}"))?;
        let mut vout = vchild.stdout.take().unwrap();
        children.lock().unwrap().push(vchild);
        let (w, h, fps) = (info.w, info.h, info.fps);
        thread::spawn(move || {
            let size = (w * h * 4) as usize;
            let mut n: u64 = 0;
            loop {
                let mut buf = vec![0u8; size];
                if vout.read_exact(&mut buf).is_err() { break; }
                let f = VFrame { pts: pos + n as f64 / fps, data: FrameData { w, h, rgba: buf } };
                n += 1;
                if tx.send(f).is_err() { break; }
            }
        });

        if let (true, Some((ring, gen, sr))) = (info.has_audio, ring) {
            let mut ac = Command::new("ffmpeg");
            ac.args(["-v", "error", "-nostdin"]);
            input_args(&mut ac, &info.audio_url.clone().unwrap_or_else(|| info.path.to_string_lossy().into_owned()), pos);
            ac
                .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", &sr.to_string(), "-"]);
            let mut achild = ac.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn().map_err(|e| e.to_string())?;
            let mut aout = achild.stdout.take().unwrap();
            children.lock().unwrap().push(achild);
            let stop2 = stop.clone();
            let cap = sr as usize * 2 * 2; // two seconds of stereo
            thread::spawn(move || {
                let mut buf = vec![0u8; 16384];
                let mut carry: Vec<u8> = Vec::new();
                while !stop2.load(Ordering::Relaxed) {
                    let n = match aout.read(&mut buf) { Ok(0) | Err(_) => break, Ok(n) => n };
                    carry.extend_from_slice(&buf[..n]);
                    let usable = carry.len() / 8 * 8; // whole stereo frames
                    let samples: Vec<f32> = carry[..usable].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                    carry.drain(..usable);
                    let mut idx = 0;
                    while idx < samples.len() && !stop2.load(Ordering::Relaxed) {
                        let mut r = ring.lock().unwrap();
                        if r.gen != gen { return; }
                        let room = cap.saturating_sub(r.q.len()) & !1;
                        if room == 0 { drop(r); thread::sleep(Duration::from_millis(8)); continue; }
                        let take = room.min(samples.len() - idx);
                        r.q.extend(&samples[idx..idx + take]);
                        idx += take;
                    }
                }
            });
        }
        Ok(DecodeSession { rx, stop, children })
    }
}

impl Drop for DecodeSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut c) = self.children.lock() {
            for ch in c.iter_mut() { let _ = ch.kill(); let _ = ch.wait(); }
        }
    }
}

/// Decode a single still frame (used for the picture while searching FF/REW).
pub fn grab_frame(info: &MediaInfo, pos: f64) -> Option<FrameData> {
    let mut c = ffmpeg_video(info, pos.max(0.0));
    // one frame only
    c.args(["-frames:v", "1"]);
    let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let size = (info.w * info.h * 4) as usize;
    if out.stdout.len() < size { return None; }
    Some(FrameData { w: info.w, h: info.h, rgba: out.stdout[..size].to_vec() })
}

/// Decode `len` seconds from `start`, one frame every `step` seconds (picture search). Frame times are `start + n*step`.
pub fn grab_range(info: &MediaInfo, start: f64, len: f64, step: f64) -> Vec<(f64, FrameData)> {
    let start = start.max(0.0);
    let mut c = Command::new("ffmpeg");
    c.args(["-v", "error", "-nostdin"]);
    input_args(&mut c, &info.path.to_string_lossy(), start);
    c.args(["-t", &format!("{len:.3}")])
        .args(["-an", "-vf", &format!("fps={:.4},scale={}:{}:flags=bilinear,format=rgba", 1.0 / step, info.w, info.h)])
        .args(["-f", "rawvideo", "-"]);
    let Ok(out) = c.stdin(Stdio::null()).stderr(Stdio::null()).output() else { return Vec::new() };
    let size = (info.w * info.h * 4) as usize;
    out.stdout.chunks_exact(size).enumerate()
        .map(|(n, b)| (start + n as f64 * step, FrameData { w: info.w, h: info.h, rgba: b.to_vec() }))
        .collect()
}

/// `-ss POS -i SRC`, with reconnect options when SRC is a network URL.
fn input_args(c: &mut Command, src: &str, pos: f64) {
    c.args(["-ss", &format!("{pos:.3}")]);
    if src.starts_with("http://") || src.starts_with("https://") {
        c.args(["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "5"]);
    }
    c.args(["-i", src]);
}

pub fn is_url(s: &str) -> bool { let s = s.trim(); s.starts_with("http://") || s.starts_with("https://") }

/// Turn a web video link (YouTube and anything else yt-dlp understands) into stream URLs.
/// Needs the `yt-dlp` program; it is called as an external tool so it can be updated on its own.
pub fn resolve_url(url: &str, max_h: u32) -> Result<MediaInfo, String> {
    let h = max_h.clamp(360, 1080);
    let fmt = format!("bv*[height<=?{h}][vcodec^=avc1]+ba/bv*[height<=?{h}]+ba/b[height<=?{h}]/b");
    let out = Command::new("yt-dlp")
        .args(["-j", "--no-playlist", "--no-warnings", "-f", &fmt, url.trim()])
        .stdin(Stdio::null()).output()
        .map_err(|e| format!("yt-dlp not found ({e}). Install it with: pip install -U yt-dlp"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| l.contains("ERROR")).unwrap_or("could not read that link");
        return Err(format!("{}. If this keeps happening, update yt-dlp: pip install -U yt-dlp", line.trim()));
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    if j["is_live"].as_bool().unwrap_or(false) { return Err("live streams are not supported".into()); }
    let formats = j["requested_formats"].as_array().cloned().unwrap_or_else(|| vec![j.clone()]);
    let has = |f: &serde_json::Value, k: &str| f[k].as_str().map(|c| c != "none").unwrap_or(false);
    let video = formats.iter().find(|f| has(f, "vcodec")).ok_or("no video stream")?;
    let audio = formats.iter().find(|f| has(f, "acodec") && !has(f, "vcodec"));
    let vurl = video["url"].as_str().ok_or("no stream url")?.to_string();
    let (sw, sh) = (video["width"].as_u64().unwrap_or(0) as u32, video["height"].as_u64().unwrap_or(0) as u32);
    if sw == 0 || sh == 0 { return Err("video has no size".into()); }
    let fps = video["fps"].as_f64().or(j["fps"].as_f64()).filter(|f| *f >= 1.0 && *f <= 120.0).unwrap_or(30.0);
    let (w, hh) = fit_size(sw, sh, max_h);
    Ok(MediaInfo {
        path: PathBuf::from(vurl),
        title: j["title"].as_str().unwrap_or("Web video").to_string(),
        duration: j["duration"].as_f64().unwrap_or(0.0),
        src_w: sw, src_h: sh, w, h: hh, fps,
        has_audio: audio.is_some() || has(video, "acodec"),
        audio_url: audio.and_then(|a| a["url"].as_str()).map(|s| s.to_string()),
    })
}
