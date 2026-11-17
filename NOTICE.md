# Third-party code

## ntsc-rs (`native/ntscrs/`)

The VHS/NTSC picture effect is the `ntscrs` crate from [ntsc-rs](https://github.com/valadaptive/ntsc-rs),
used under its MIT / ISC / Apache-2.0 licences (the licence files are in `native/ntscrs/`).

It has been modified for this project:

* `tracking_bars`: a list of extra tracking-noise bands that can sit at any height (the same noise code as
  ntsc-rs's bottom-of-picture tracking noise, with a position, a gain and a seed salt).
* A "Bar position" tracking-noise setting (`TrackingLayout`: Bottom or Middle), setting id 62.

Everything else in that folder is unchanged. The bundled `Cargo.toml` is trimmed (no benches or dev-dependencies).

## Used as external programs (not included)

* `ffmpeg` / `ffprobe` decode the video and audio.
* `yt-dlp` (optional) resolves web video links.
* `zenity` provides the file dialogs.

## Rust dependencies

eframe/egui, cpal, png and serde_json, each under its own licence (see their crates).

## Screenshot credits

The images in `docs/screenshots/` are frames from *Sintel* (c) Blender Foundation, https://durian.blender.org, licensed CC BY 3.0 (https://creativecommons.org/licenses/by/3.0/). They have been modified by applying the VHS effect.
