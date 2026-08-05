# FastTrackStudios fork of nice-plug

Fork of [RustAudio/nice-plug](https://codeberg.org/RustAudio/nice-plug)
carrying the FastTrackStudio additions.

**Base: upstream's `baseview-03` branch** (upstream `main` + the big
editor-API rework + baseview host callbacks for CLAP and VST3), not
upstream `main`.

## Why baseview 0.3

baseview 0.3 adds `baseview::host::HostCallbacks::request_resize`, which
lets a child window ask the DAW to resize the parent. That is what makes
plugin-driven window resizing possible, and the FTS editors need it:
several of their surfaces do not fit a fixed window, and Blitz collapses
what overflows instead of scrolling it.

The old vendored `vendor/baseview` (a raw-window-handle-0.6 fork of
baseview 0.1) is gone — 0.3 links rwh 0.6 natively, so every crate here
now shares the same crates.io baseview.

## The FTS delta

- **`crates/nice-plug-dioxus`** — Dioxus Native GUI backend
  (Blitz + Vello + wgpu, softbuffer/CPU fallbacks, hot reload,
  standalone windows via `open_standalone_with_state`), ported to
  baseview 0.3 and the reworked `Editor`/`EditorHandle` split.
  `DioxusState::with_resize_hint` opts an editor into host resizing.
- **Embedded editor API** (`nice-plug-core/src/editor/embedded.rs`) +
  REAPER inline FX UI (`cockos.reaper_embedui`) CLAP extension.
- **Rich `GuiContext::track_info()`** (name/color/channels/bus flags,
  CLAP `track-info/1`) — also feeds upstream's `Plugin::track_info_updated`.
- **Param rescan requests** (`rescan_param_info` / `rescan_param_all`).
- **`ActivateContext::raw_host_context()`** — raw `clap_host*` for
  DAW-specific APIs (REAPER extension pointer).
- Prelude: upstream's plugin `TrackInfo` is re-exported as
  `PluginTrackInfo`; the gui `TrackInfo` keeps the unqualified name.

## Divergences from upstream's branch

Two things upstream's `baseview-03` branch left in a state that does not
build anywhere but their machine:

- `nice-plug-egui` pointed `egui-baseview` at a sibling checkout
  (`../../../egui-baseview`). Repointed at that repo's matching
  `baseview-03` branch. Revert to a released version once one exists.
- `nice-plug-iced` was not ported to the reworked `Editor` trait and does
  not compile. Excluded from the workspace — FTS ships the dioxus backend
  and has no way to test iced. Restore when upstream ports it.

## Status

`cargo check --workspace` is green. The baseview 0.3 port is
**compile-clean but not yet exercised at runtime** — opening an editor in
a host, and confirming resize actually negotiates with the DAW, is the
next step.

## Policy

**Nothing here is submitted upstream until it is 100% ready — that
decision is Cody's alone.**

Consumed by the FastTrackStudio monorepo as a git dependency.
