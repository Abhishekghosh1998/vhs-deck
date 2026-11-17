mod app;
mod audio_out;
mod media;
mod process;

use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use vhs_core::demo::demo_frame;
use vhs_core::params::Cond;

fn arg_f(args: &[String], name: &str, default: f32) -> f32 {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().map_err(|e| e.to_string())?.write_image_data(rgba).map_err(|e| e.to_string())
}

/// `vhs-deck --render-test out.png [--cond 0|1|2] [--track X] [--t SECONDS] [--h LINES] [--frame N] [--source FILE]`
/// Runs the demo tape (or one frame of FILE) through the real ntsc-rs picture path without opening a window.
fn render_test(args: &[String]) -> Result<(), String> {
    let out = args.iter().position(|a| a == "--render-test").and_then(|i| args.get(i + 1)).ok_or("missing output path")?;
    let cond = Cond::from_idx(arg_f(args, "--cond", 0.0) as usize);
    let track = arg_f(args, "--track", 0.0);
    let t = arg_f(args, "--t", 4.0);
    let h = arg_f(args, "--h", 480.0) as usize;
    let frame_num = arg_f(args, "--frame", 10.0) as usize;
    let src = if let Some(p) = args.iter().position(|a| a == "--source").and_then(|i| args.get(i + 1)) {
        let info = media::probe(std::path::Path::new(p), h as u32)?;
        media::grab_frame(&info, t as f64).ok_or("could not decode a frame")?
    } else {
        let w = h * 4 / 3;
        let mut rgba = Vec::new();
        demo_frame(t, w, h, &mut rgba);
        media::FrameData { w: w as u32, h: h as u32, rgba }
    };
    let pause = args.iter().any(|a| a == "--pause");
    let search = if pause { 1.0 } else { arg_f(args, "--search", 0.0) };
    let ss = process::SearchSettings::default();
    let base = process::preset(cond);
    let (bars, v_shift, strikes, effect);
    if search != 0.0 {
        bars = process::search_bars(&ss, if pause { 0.0 } else { search }, t, src.h as usize, track);
        v_shift = ss.drop_pct / 100.0; strikes = ss.strikes;
        effect = process::search_effect(&base, &ss, bars, if pause { 1.0 } else { ss.resting_noise });
    } else {
        bars = process::tracking_bars(&base, track, t, src.h as usize);
        v_shift = 0.0; strikes = 0.0;
        effect = process::with_bars(&base, bars);
    }
    let t0 = std::time::Instant::now();
    let mut o = None;
    let runs = arg_f(args, "--runs", 1.0) as usize;
    for i in 0..runs.max(1) {
        o = Some(process::process(&process::Job {
            scale: [arg_f(args, "--sx", 1.0), arg_f(args, "--sy", 1.0)],
            v_shift, strikes,
            src: Arc::new(media::FrameData { w: src.w, h: src.h, rgba: src.rgba.clone() }),
            effect: effect.clone(), frame_num: frame_num + i, drops: [[0.0; 4]; 8], flag: 0.0, time: t, bypass: false,
        }));
    }
    let o = o.unwrap();
    write_png(out, o.w as u32, o.h as u32, &o.rgba)?;
    println!("wrote {out} ({}x{}), {:.1} ms/frame over {} run(s)", o.w, o.h, t0.elapsed().as_secs_f64() * 1000.0 / runs.max(1) as f64, runs.max(1));
    Ok(())
}

/// `vhs-deck --probe FILE`: exercises ffprobe, the video/audio decoders, the audio ring and seeking without a window.
fn probe_test(path: &str) -> Result<(), String> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    let info = if media::is_url(path) { media::resolve_url(path, 480)? } else { media::probe(std::path::Path::new(path), 480)? };
    println!("probe: {} {}x{} (decoding at {}x{}) @ {:.3} fps, {:.2}s, audio={}", info.title, info.src_w, info.src_h, info.w, info.h, info.fps, info.duration, info.has_audio);
    for (label, pos) in [("start", 0.0), ("seek", (info.duration * 0.5).max(0.0))] {
        let ring = Arc::new(Mutex::new(audio_out::Ring { gen: 1, q: Default::default() }));
        let dec = media::DecodeSession::start(&info, pos, Some((ring.clone(), 1, 48000)))?;
        let t0 = Instant::now();
        let (mut frames, mut first, mut last) = (0u32, f64::NAN, 0.0f64);
        while t0.elapsed() < Duration::from_millis(1500) && frames < 30 {
            if let Ok(f) = dec.rx.recv_timeout(Duration::from_millis(500)) {
                if frames == 0 { first = f.pts; }
                last = f.pts;
                frames += 1;
            } else { break; }
        }
        std::thread::sleep(Duration::from_millis(300));
        let samples = ring.lock().unwrap().q.len();
        println!("{label:>5} @{pos:.2}s: {frames} frames pts {first:.3}..{last:.3}, {samples} audio samples buffered ({:.2}s)", samples as f64 / 2.0 / 48000.0);
        if frames == 0 { return Err("no frames decoded".into()); }
        if info.has_audio && samples == 0 { return Err("no audio decoded".into()); }
    }
    let still = media::grab_frame(&info, info.duration * 0.25).ok_or("grab_frame failed")?;
    let t0 = Instant::now();
    let r = media::grab_range(&info, info.duration * 0.2, 4.0, 0.25);
    println!("search chunk: {} frames (pts {:.2}..{:.2}) in {:.2}s", r.len(), r.first().map(|x| x.0).unwrap_or(0.0), r.last().map(|x| x.0).unwrap_or(0.0), t0.elapsed().as_secs_f64());
    if r.len() < 10 { return Err("search chunk too short".into()); }
    println!("search still: {}x{} ({} bytes)", still.w, still.h, still.rgba.len());
    Ok(())
}

/// Window / taskbar icon, decoded from the bundled PNG.
fn app_icon() -> egui::IconData {
    let bytes = include_bytes!("../../../assets/icon-128.png");
    let mut reader = match png::Decoder::new(&bytes[..]).read_info() { Ok(r) => r, Err(_) => return Default::default() };
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) { Ok(i) => i, Err(_) => return Default::default() };
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => return Default::default(),
    };
    egui::IconData { rgba, width: info.width, height: info.height }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--render-test") {
        if let Err(e) = render_test(&args) { eprintln!("render test failed: {e}"); std::process::exit(1); }
        return Ok(());
    }
    if let Some(p) = args.iter().position(|a| a == "--probe").and_then(|i| args.get(i + 1)) {
        if let Err(e) = probe_test(p) { eprintln!("probe test failed: {e}"); std::process::exit(1); }
        return Ok(());
    }
    let initial = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VHS Deck")
            .with_app_id("vhs-deck")
            .with_icon(app_icon())
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("VHS Deck", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, initial)))))
}
