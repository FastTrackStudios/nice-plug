# FastTrackStudios fork of nice-plug

Fork of [RustAudio/nice-plug](https://codeberg.org/RustAudio/nice-plug)
carrying the FastTrackStudio additions:

- **`crates/nice-plug-dioxus`** — Dioxus Native GUI backend
  (Blitz + Vello + wgpu, softbuffer/CPU fallbacks, hot reload,
  standalone windows via `open_standalone_with_state`).
- **Embedded editor API** (`nice-plug-core/src/editor/embedded.rs`) +
  REAPER inline FX UI (`cockos.reaper_embedui`) CLAP extension.
- **Rich `GuiContext::track_info()`** (name/color/channels/bus flags,
  CLAP `track-info/1`) — also feeds upstream's `Plugin::track_info_updated`.
- **Param rescan requests** (`rescan_param_info` / `rescan_param_all`).
- **`InitContext::raw_host_context()`** — raw `clap_host*` for
  DAW-specific APIs (REAPER extension pointer).
- `vendor/baseview` — the pinned raw-window-handle-0.6 baseview fork used
  by nice-plug-dioxus's own windows (rest of the workspace is on
  crates.io baseview 0.2).

Based on upstream `main` + upstream PR #61 (baseview 0.2 update).

**Policy: nothing here is submitted upstream until it is 100% ready —
that decision is Cody's alone.**

Consumed by the FastTrackStudio monorepo as a git dependency.
