//! Scene overlay support for embedding custom GPU-rendered content in Dioxus UI.
//!
//! This module provides the `use_scene_overlay` hook and re-exports the `SceneOverlay` trait
//! from the renderer, allowing you to embed custom vello-rendered content (like EQ graphs,
//! spectrum analyzers, oscilloscopes) within your Dioxus plugin UI.
//!
//! Scene overlays paint directly into the main vello Scene after the Dioxus DOM is rendered,
//! giving full access to vello's 2D vector graphics (anti-aliased curves, gradients, glow effects).
//!
//! # Element-relative positioning
//!
//! Overlays can be positioned relative to a DOM element by calling `set_rect()` on the
//! returned `OverlayHandle`. The overlay's `paint()` receives element-local coordinates
//! (width/height of the rect), while the renderer handles the window-space transform.
//!
//! # Example
//!
//! ```ignore
//! use nice_plug_dioxus::prelude::*;
//! use nice_plug_dioxus::custom_paint::{use_scene_overlay, SceneOverlay};
//!
//! struct MyOverlay { /* shared state */ }
//!
//! impl SceneOverlay for MyOverlay {
//!     fn paint(&mut self, scene: &mut vello::Scene, transform: Affine, width: u32, height: u32, scale: f64) {
//!         // Use `transform` for all scene.fill()/stroke() calls.
//!         // width/height are the overlay rect dimensions in CSS pixels.
//!     }
//! }
//!
//! #[component]
//! fn MyView() -> Element {
//!     let overlay = use_scene_overlay(|| MyOverlay::new());
//!     // Position the overlay at (10, 50) with size 800x400 CSS pixels
//!     overlay.set_rect(10.0, 50.0, 800.0, 400.0);
//!     rsx!(div { style: "width:100%; height:100%;", /* interaction handling */ })
//! }
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use dioxus_native::prelude::dioxus_core::use_hook_with_cleanup;
use dioxus_native::prelude::*;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;

// Re-export the SceneOverlay trait and vello types for users
pub use crate::renderer::SceneOverlay;
pub use vello;

/// Rectangle in CSS (logical) pixels, relative to the window origin.
#[derive(Clone, Debug, Default)]
pub struct OverlayRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Whether an overlay paints behind or in front of the DOM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayLayer {
    /// Paint before the DOM (behind knobs, text, etc.)
    Background,
    /// Paint after the DOM (on top of everything).
    #[default]
    Foreground,
}

/// Internal entry for a registered overlay.
struct OverlayEntry {
    id: u64,
    overlay: Box<dyn SceneOverlay>,
    /// Element-relative rect (CSS pixels). `None` = paint in full window space.
    rect: Option<OverlayRect>,
    layer: OverlayLayer,
}

/// Handle to the renderer's overlay registry.
///
/// Provided as a Dioxus context so that `use_scene_overlay` can register overlays
/// at component mount time. The actual rendering happens in the Renderer's render loop.
#[derive(Clone)]
pub struct OverlayRegistry {
    inner: Rc<RefCell<OverlayRegistryInner>>,
}

struct OverlayRegistryInner {
    entries: Vec<OverlayEntry>,
    next_id: u64,
}

impl Default for OverlayRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayRegistry {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(OverlayRegistryInner {
                entries: Vec::new(),
                next_id: 1,
            })),
        }
    }

    /// Register a scene overlay (foreground by default) and return its unique ID.
    pub fn register(&self, overlay: Box<dyn SceneOverlay>) -> u64 {
        self.register_with_layer(overlay, OverlayLayer::Foreground)
    }

    /// Register a background overlay (paints behind the DOM).
    pub fn register_background(&self, overlay: Box<dyn SceneOverlay>) -> u64 {
        self.register_with_layer(overlay, OverlayLayer::Background)
    }

    pub fn register_with_layer(&self, overlay: Box<dyn SceneOverlay>, layer: OverlayLayer) -> u64 {
        let mut inner = self.inner.borrow_mut();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(OverlayEntry {
            id,
            overlay,
            rect: None,
            layer,
        });
        id
    }

    /// Unregister a scene overlay by ID.
    pub fn unregister(&self, id: u64) {
        let mut inner = self.inner.borrow_mut();
        inner.entries.retain(|e| e.id != id);
    }

    /// Update the overlay's position and size (CSS pixels, window-relative).
    ///
    /// When set, the renderer will:
    /// 1. Apply a translate+scale transform so the overlay paints in element-local coords
    /// 2. Clip to the rect boundaries
    /// 3. Pass the rect width/height (not window size) to `paint()`
    pub fn set_rect(&self, id: u64, rect: OverlayRect) {
        let mut inner = self.inner.borrow_mut();
        if let Some(entry) = inner.entries.iter_mut().find(|e| e.id == id) {
            entry.rect = Some(rect);
        }
    }

    /// Paint all registered overlays into the scene.
    /// Called by the Renderer each frame.
    pub fn paint_all(&self, scene: &mut vello::Scene, width: u32, height: u32, scale: f64) {
        self.paint_layer(scene, width, height, scale, None);
    }

    /// Paint overlays for a specific layer (or all if `layer` is `None`).
    pub fn paint_layer(
        &self,
        scene: &mut vello::Scene,
        width: u32,
        height: u32,
        scale: f64,
        layer: Option<OverlayLayer>,
    ) {
        let mut inner = self.inner.borrow_mut();
        let mut painted = 0usize;
        let mut skipped_zero = 0usize;
        for entry in &mut inner.entries {
            if let Some(filter) = layer {
                if entry.layer != filter {
                    continue;
                }
            }
            if let Some(rect) = &entry.rect {
                // Skip zero-size rects
                if rect.width < 1.0 || rect.height < 1.0 {
                    skipped_zero += 1;
                    continue;
                }

                // Transform: position at (rect.x, rect.y) in window, scaled by display factor
                let transform =
                    Affine::translate((rect.x * scale, rect.y * scale)) * Affine::scale(scale);

                // Clip to the overlay rect (in element-local CSS coords, pre-transform)
                let clip = Rect::new(0.0, 0.0, rect.width, rect.height);

                // Vello 0.8 added a `clip_style` arg in front of the existing
                // (transform, clip) pair.
                scene.push_clip_layer(Fill::NonZero, transform, &clip);

                // Paint in element-local coordinates — pass transform so
                // painters draw in the correct window position.
                entry.overlay.paint(
                    scene,
                    transform,
                    rect.width as u32,
                    rect.height as u32,
                    scale,
                );
                painted += 1;

                scene.pop_layer();
            } else {
                // No rect set — paint in full window space (legacy mode)
                let identity = Affine::IDENTITY;
                entry.overlay.paint(scene, identity, width, height, scale);
                painted += 1;
            }
        }
        if diagnostics_enabled() && (painted > 0 || skipped_zero > 0) {
            eprintln!(
                "[NIH_DIOXUS_DIAG] overlay layer={layer:?} entries={} painted={painted} skipped_zero={skipped_zero} window={}x{} scale={scale}",
                inner.entries.len(),
                width,
                height
            );
        }
    }
}

fn diagnostics_enabled() -> bool {
    std::env::var_os("NIH_DIOXUS_DIAGNOSTICS").is_some()
}

/// Handle returned by `use_scene_overlay` for controlling an overlay's position.
///
/// When `OverlayRegistry` is not in context (e.g. standalone `dioxus_native` mode),
/// the handle is a no-op — `set_rect` does nothing.
#[derive(Clone)]
pub struct OverlayHandle {
    registry: Option<OverlayRegistry>,
    id: u64,
}

impl OverlayHandle {
    /// Set the overlay's position and size in CSS (logical) pixels, relative to the window.
    ///
    /// Call this each render to keep the overlay positioned correctly as the layout changes.
    /// No-op when no `OverlayRegistry` is present in context.
    pub fn set_rect(&self, x: f64, y: f64, width: f64, height: f64) {
        if let Some(registry) = &self.registry {
            registry.set_rect(
                self.id,
                OverlayRect {
                    x,
                    y,
                    width,
                    height,
                },
            );
        }
    }
}

/// Register a scene overlay that renders custom vello content each frame.
///
/// Returns an `OverlayHandle` that you MUST call `set_rect()` on each render
/// to position the overlay within the window.
///
/// The overlay's `paint()` method receives element-local coordinates —
/// (0,0) is the top-left of the rect, and width/height are the rect dimensions.
///
/// The overlay is automatically unregistered when the component unmounts.
///
/// # Arguments
///
/// * `create_overlay` - A closure that creates the overlay. Called once on first render.
pub fn use_scene_overlay<T: SceneOverlay>(create_overlay: impl FnOnce() -> T) -> OverlayHandle {
    use_scene_overlay_on_layer(create_overlay, OverlayLayer::Foreground)
}

/// Like [`use_scene_overlay`] but paints behind the DOM (background layer).
///
/// Use this for waveform displays, spectrums, or other visualizations
/// that should appear behind interactive DOM elements like knobs.
pub fn use_scene_overlay_background<T: SceneOverlay>(
    create_overlay: impl FnOnce() -> T,
) -> OverlayHandle {
    use_scene_overlay_on_layer(create_overlay, OverlayLayer::Background)
}

pub fn use_scene_overlay_on_layer<T: SceneOverlay>(
    create_overlay: impl FnOnce() -> T,
    layer: OverlayLayer,
) -> OverlayHandle {
    let (registry, id): (Option<OverlayRegistry>, u64) = use_hook_with_cleanup(
        move || -> (Option<OverlayRegistry>, u64) {
            let registry: Option<OverlayRegistry> = try_consume_context::<OverlayRegistry>();
            let id: u64 = if let Some(ref reg) = registry {
                let overlay = Box::new(create_overlay());
                reg.register_with_layer(overlay, layer)
            } else {
                // No registry in context (e.g. standalone dioxus_native / dx serve).
                // Drop the overlay and return a no-op id.
                drop(create_overlay());
                0
            };
            (registry, id)
        },
        |(registry, id): (Option<OverlayRegistry>, u64)| {
            if let Some(reg) = registry {
                reg.unregister(id);
            }
        },
    );
    OverlayHandle {
        registry: registry.clone(),
        id,
    }
}
