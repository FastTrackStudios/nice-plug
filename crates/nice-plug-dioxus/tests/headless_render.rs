//! Renders an editor component headlessly through the same pipeline a plugin
//! window uses — Dioxus document, Blitz layout and paint, Vello on wgpu — and
//! checks the pixels that come back.
//!
//! Needs a GPU adapter (any wgpu backend, a software one included). There is
//! no window and no display connection.

use nice_plug_dioxus::prelude::*;
use nice_plug_dioxus::render_screenshot;

const W: u32 = 64;
const H: u32 = 48;

#[component]
fn Halves() -> Element {
    rsx! {
        div { style: "position: absolute; left: 0; top: 0; width: 64px; height: 24px; background-color: rgb(255, 0, 0);" }
        div { style: "position: absolute; left: 0; top: 24px; width: 64px; height: 24px; background-color: rgb(0, 0, 255);" }
    }
}

/// The RGBA pixel at (x, y).
fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[test]
fn a_component_renders_to_the_pixels_its_css_asks_for() {
    let rgba = render_screenshot(Halves, W, H, None);
    assert_eq!(rgba.len(), (W * H * 4) as usize);

    assert_eq!(pixel(&rgba, 32, 10), [255, 0, 0, 255], "top half is red");
    assert_eq!(
        pixel(&rgba, 32, 36),
        [0, 0, 255, 255],
        "bottom half is blue"
    );
}

#[component]
fn Shared() -> Element {
    let shared = use_context::<SharedState>();
    let colour = shared
        .get::<&'static str>()
        .expect("shared state in context");
    rsx! {
        div { style: "position: absolute; inset: 0; background-color: {colour};" }
    }
}

/// Shared state reaches components in the headless path, as it does in the
/// editor window — the context wiring did not move with the Dioxus bump.
#[test]
fn shared_state_reaches_the_component() {
    let state = SharedState::new(std::sync::Arc::new("rgb(0, 255, 0)"));
    let rgba = render_screenshot(Shared, W, H, Some(state));
    assert_eq!(pixel(&rgba, 5, 5), [0, 255, 0, 255]);
}
