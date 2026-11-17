# VHS Deck

A VLC-style video player that makes any video look and sound like it is playing from a worn VHS tape.

* **Picture:** the real [ntsc-rs](https://github.com/valadaptive/ntsc-rs) NTSC/VHS effect, run on every frame.
* **Tracking:** one tracking control driving ntsc-rs-style snow bars, at the bottom or splitting out from the middle.
* **Transport:** fast-forward / rewind picture search with real moving frames, noise bars and strike lines; pause with a dropped picture and snow strip; picture roll, head switching, dropouts, sync flagging.
* **Sound:** a tape audio chain (wow/flutter, bandwidth loss, hiss, dropouts) and, with bad tracking, chopped-out audio instead of noise.
* **Player features:** aspect ratio, zoom, processing resolution (480 to 1080+), independent X/Y picture scale, and playback from web links.

![platform](https://img.shields.io/badge/platform-linux%20x86__64-blue) ![license](https://img.shields.io/badge/license-MIT-green)

## Download and run (Linux x86_64)

1. Download `vhs-deck-<version>-linux-x86_64.tar.gz` from the **Releases** page.
2. Install the runtime tools (Debian/Ubuntu shown):

       sudo apt install ffmpeg zenity libasound2

3. Unpack and run:

       tar xzf vhs-deck-*-linux-x86_64.tar.gz
       cd vhs-deck-*-linux-x86_64
       ./vhs-deck /path/to/video.mp4

   Or run `./vhs-deck` and use **Media → Open File**, or drag a file onto the window.

Requirements: a glibc 2.35+ distribution (Ubuntu 22.04+, Debian 12+, Fedora 36+ or newer), an X11 or Wayland desktop with OpenGL, and a working sound device (ALSA, PulseAudio or PipeWire).

Optional, for web links: `yt-dlp` (see [Web videos](#web-videos-and-responsibility)).

## Build from source (Linux x86_64)

### 1. Install the build dependencies

Debian / Ubuntu:

    sudo apt install build-essential pkg-config libasound2-dev ffmpeg zenity git curl

Fedora:

    sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel ffmpeg zenity git curl

Arch:

    sudo pacman -S base-devel pkgconf alsa-lib ffmpeg zenity git curl

### 2. Install Rust (1.85 or newer, the vendored ntsc-rs uses edition 2024)

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    source ~/.cargo/env
    rustup update stable

### 3. Build and run

    git clone <this repository>
    cd vhs-deck/native
    cargo build --release -p vhs-deck
    ./target/release/vhs-deck /path/to/video.mp4

Or in one step: `cargo run --release -p vhs-deck -- /path/to/video.mp4`. Always use `--release`; the effect is far too slow in a debug build.

The graphics and windowing libraries (X11, Wayland, OpenGL) are loaded at run time, so no `-dev` packages are needed for them.

### 4. Make a release tarball

    scripts/package.sh

This writes `dist/vhs-deck-<version>-linux-x86_64.tar.gz` and a `.sha256` file. Pushing a tag such as `v0.1.0` runs `.github/workflows/release.yml`, which builds the tarball on Ubuntu 22.04 and attaches it to a GitHub release. Build release binaries on the oldest distro you want to support, since the binary needs at least the glibc it was built against.

### 5. Tests

    cd native && cargo test --release -p vhs-core

## Using it

Web videos: **Media → Open URL…**, Ctrl+V with a link copied, or pass the link on the command line.

* Menus: Media, Playback, Audio, Video (Aspect Ratio, Scale/Zoom, Processing resolution, Scaling filter), VCR, View.
* Aspect ratio: Default, 16:9, 4:3, 1:1, 16:10, 2.21:1, 2.35:1, 2.39:1, 5:4.
* Scale/Zoom: Fit, Fill (crop), Stretch, 1:4, 1:2, 1:1, 2:1.
* Keys: Space play/pause, ←/→ ±5 s, ↑/↓ volume, M mute, F fullscreen, Ctrl+O open, `[` `]` tracking.

Side panel, tab **VCR**:

* **Resolution:** lines the video is scaled to before the VHS effect (480 default, 1080 for an "HD VHS" look, Source, or any value).
* **Picture scale:** X and Y stretch about the centre, applied before the VHS effect. Link them to scale together, or unlink to set them independently, for example X=1, Y=1.45 to crop away letterbox bars.
* **Tape:** condition, speed SP/LP/EP, tracking −/+ with auto, video noise, head switching, colour bleed, picture roll, audio hiss, wow & flutter, audio bandwidth. These write into the real ntsc-rs settings.

Tab **FF / REW**: number of bars, bar height, roll speed, snow, anisotropy, noise, wave, strike lines, picture drop, and how much normal tape noise is kept. Independent of the tracking settings; pause uses the same settings with one bar.

Tab **All ntsc-rs settings**: every ntsc-rs setting, generated from ntsc-rs's own list, with load/save of ntsc-rs JSON presets.

## How it works

* `native/ntscrs` is the vendored ntsc-rs effect crate (MIT/ISC/Apache-2.0, licences included), modified to add `tracking_bars` and a "Bar position" setting (Bottom, or Middle splitting toward top and bottom). See `NOTICE.md`.
* Tracking is one knob plus one set of looks (ntsc-rs settings → Tracking noise). The ideal tracking position drifts over time, so it has to be retuned; AUTO follows it.
* Fast-forward and rewind play real frames at about 6x.
* Bad tracking audio: no hiss and no pitch change; the sound chops in and out at the video field rate and loses treble while out.
* Picture roll, dropouts and sync flagging are not part of ntsc-rs; they live in `vhs-core/src/sim.rs` and `vhs-deck/src/process.rs`. The audio chain is in `vhs-core/src/audio.rs`.
* Decoding uses `ffmpeg`/`ffprobe` through pipes; the audio buffer is the master clock.

## Headless checks

    vhs-deck --render-test out.png --cond 2 --track 0.5 --h 720     # demo picture through ntsc-rs
    vhs-deck --render-test out.png --sx 1 --sy 1.45                 # with X/Y picture scale
    vhs-deck --probe video.mp4                                      # decoder, audio and seek self-test

`web/index.html` is an earlier browser prototype and does not use ntsc-rs.

## Web videos and responsibility

The optional "Open URL" feature streams a web video through `yt-dlp`, which is **not** included and must be installed separately (`pip install -U yt-dlp`; keep it updated, since sites change often). Nothing is saved to disk. Stream links expire after a few hours, and seeking and FF/REW are slower over the network. You are responsible for following the terms of service of any site you play from, and for having the right to watch the content. This project is not affiliated with YouTube or Google.

## Troubleshooting

* **Error about `libasound.so.2`:** install `libasound2` (Debian/Ubuntu) or `alsa-lib` (Fedora/Arch).
* **`GLIBC_2.35 not found`:** your distro is older than the release build; build from source.
* **No file dialog:** install `zenity`. Dropping a file on the window works without it.
* **Slow or stuttering picture:** lower the resolution (VCR tab); the effect runs on the CPU.
* **Web link fails:** update `yt-dlp`.

## Licence

MIT, see `LICENSE`. The ntsc-rs code in `native/ntscrs/` keeps its own licences and is modified, see `NOTICE.md`. Thanks to the ntsc-rs project for the VHS/NTSC emulation.
