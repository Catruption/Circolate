# Circolate

Circolate is a low-end type tracker-player for all types of source files like FLP, VGM, and MIDI, turning them into a readable language (HALOTEXT) that can be more easily edited and shared.

Halotext can support just about anything, but FLP conversion is still a little early cause FL Studio is very obtuse and I'm not a producer.

This project really is one of my long-standing dreams, is filled with a lot of my past, and will become my future. I hope you enjoy it and it becomes your angel too.

[![Circolate Demo](https://img.youtube.com/vi/bj--WA7TyZQ/0.jpg)](https://www.youtube.com/watch?v=bj--WA7TyZQ)

## What it does

*   **Plays and converts** VGM, MIDI, and FLP files into `.halo.txt` (Halotext).
*   **Visualizes** playback in the terminal with a TUI, spiral visualizer, and braille waveforms.
*   **Connects** to VS Code and Kate for a live, auto-scrolling playhead while you edit.
*   **Archives** projects into portable `.hlz` files (which are just zipped folders with a manifest).

## Building

Requires the VST3 SDK built at `/opt/vst3sdk` (or set `VST3_SDK` to your SDK path):

```bash
git clone --recursive https://github.com/steinbergmedia/vst3sdk.git /opt/vst3sdk
cd /opt/vst3sdk
cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DSMTG_ENABLE_VST3_HOSTING_EXAMPLES=OFF \
    -DSMTG_ENABLE_VST3_PLUGIN_EXAMPLES=OFF \
    -DSMTG_ENABLE_VSTGUI_SUPPORT=OFF
cmake --build build --target sdk_hosting sdk_common sdk pluginterfaces base
```

Then build Circolate:

```bash
cargo build --release
```
