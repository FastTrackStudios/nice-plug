//! Dioxus Native GUI support for nice-plug.
//!
//! This crate provides a native GPU-rendered UI framework for audio plugins
//! using Dioxus with the Blitz rendering engine (Vello + wgpu).
//!
//! # Features
//!
//! - **Windowed Editor**: Full Dioxus UI in a resizable window (using baseview)
//! - **Embedded Editor**: Render Dioxus UI to REAPER's TCP/MCP inline display
//! - **Custom WGPU**: Embed custom GPU-rendered content (spectrum analyzers, etc.)
//!
//! # Example - Windowed Editor
//!
//! ```ignore
//! use nice_plug_dioxus::prelude::*;
//!
//! fn editor(
//!     params: Arc<MyParams>,
//!     editor_state: Arc<DioxusState>,
//! ) -> Option<Box<dyn Editor>> {
//!     create_dioxus_editor(editor_state, App)
//! }
//!
//! #[component]
//! fn App() -> Element {
//!     rsx! {
//!         div { class: "plugin-ui",
//!             h1 { "My Plugin" }
//!         }
//!     }
//! }
//! ```
//!
//! # Example - Embedded Editor (REAPER TCP/MCP)
//!
//! ```ignore
//! use nice_plug_dioxus::embedded::DioxusEmbeddedEditor;
//!
//! impl Plugin for MyPlugin {
//!     fn embedded_editor(&mut self) -> Option<Arc<dyn EmbeddedEditor>> {
//!         Some(Arc::new(DioxusEmbeddedEditor::new(
//!             self.dioxus_state.clone(),
//!             App,
//!         )))
//!     }
//! }
//! ```
//!
//! # Example - Custom WGPU Canvas
//!
//! ```ignore
//! use nice_plug_dioxus::prelude::*;
//! use nice_plug_dioxus::custom_paint::{use_wgpu, CustomPaintSource};
//!
//! #[component]
//! fn SpectrumView() -> Element {
//!     let paint_source = SpectrumPaintSource::new();
//!     let canvas_id = use_wgpu(move || paint_source);
//!     
//!     rsx!(
//!         canvas { id: "spectrum", "src": canvas_id }
//!     )
//! }
//! ```

#![allow(clippy::type_complexity)]

use std::any::Any;
use std::sync::Arc;

use dioxus_native::prelude::Element;
use nice_plug_core::editor::Editor;

// Re-export dioxus_native for convenience
pub use dioxus_native;

// Re-export individual dioxus crates for more specific imports
pub use dioxus_native::prelude::dioxus_core;
pub use dioxus_native::prelude::dioxus_elements;
pub use dioxus_native::prelude::dioxus_signals;
pub use dioxus_native::prelude::document;

// Re-export wgpu so consumers share this crate's exact version.
pub use wgpu;

/// Custom-paint `<canvas>` support for the standalone / dx-serve path, using
/// anyrender 0.10's texture-registration model.
///
/// To back a `<canvas src="{id}">` with GPU content: pull the active
/// [`DioxusNativeWindowRenderer`] from Dioxus context, obtain its
/// [`DeviceHandle`] via [`RenderContext::renderer_specific_context`], create a
/// `wgpu::Texture`, register it with
/// [`RenderContext::try_register_custom_resource`] (pass `Box::new(texture)`),
/// and use the returned [`ResourceId`]'s `into_ffi()` as the canvas `src`. Render
/// into the texture on your own schedule; the renderer composites it each frame.
/// Call [`RenderContext::unregister_resource`] on teardown.
pub mod wgpu_canvas {
    pub use anyrender::{RenderContext, ResourceId};
    pub use anyrender_vello::DeviceHandle;
    pub use dioxus_native::DioxusNativeWindowRenderer;
}

/// Blitz native custom-DOM-widget support (the `custom-widget` mechanism).
///
/// A DOM node becomes a custom-painted widget whose [`Widget::paint`] records
/// into an anyrender [`Scene`] that blitz composites into its own paint pass at
/// the node's layout box — no separate renderer, offscreen texture, `<canvas>`,
/// or scene-overlay required, and it works identically in the plugin editor and
/// standalone. Attach it declaratively to an `object` element:
///
/// ```ignore
/// use nice_plug_dioxus::widget::{CustomWidgetAttr, Widget, Scene, PaintScene as _};
///
/// let w = use_memo(|| CustomWidgetAttr::new(MyWidget::new()));
/// rsx! { object { "data": w } }
/// ```
///
/// Implement [`Widget::paint`] to draw via the [`PaintScene`] trait (vello is the
/// backend), [`Widget::handle_event`] for input ([`UiEvent`]), and
/// [`Widget::can_create_surfaces`] to grab the wgpu device/queue for GPU-texture
/// widgets via the [`RenderContext`].
pub mod widget {
    pub use anyrender::{PaintScene, RenderContext, Scene};
    pub use blitz_dom::node::ComputedStyles;
    pub use blitz_traits::events::UiEvent;
    pub use dioxus_native::{CustomWidgetAttr, Widget};
}

/// A type-erased wrapper for shared UI state.
///
/// This allows the framework to store arbitrary state types while preserving
/// the ability to downcast them in components.
///
/// # Usage in components
///
/// ```ignore
/// use nice_plug_dioxus::prelude::*;
///
/// #[component]
/// fn App() -> Element {
///     // Get the wrapper from context
///     let shared = use_context::<SharedState>();
///     // Downcast to your concrete type
///     let ui_state = shared.get::<MyUiState>().expect("MyUiState not in context");
///     // ...
/// }
/// ```
#[derive(Clone)]
pub struct SharedState {
    inner: Arc<dyn Any + Send + Sync>,
}

impl SharedState {
    /// Create a new SharedState wrapper around the given value.
    pub fn new<T: Any + Send + Sync + 'static>(value: Arc<T>) -> Self {
        Self { inner: value }
    }

    /// Try to downcast to the concrete type.
    /// Returns the Arc<T> if the type matches, None otherwise.
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        // Clone the Arc and try to downcast it
        self.inner.clone().downcast::<T>().ok()
    }

    /// Try to get a reference to the inner value.
    pub fn get_ref<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        self.inner.downcast_ref::<T>()
    }
}

// Public modules
pub mod assets;
pub mod context;
pub mod custom_paint;
#[cfg(feature = "embedded")]
pub mod embedded;
pub mod widgets;

// Internal modules
mod editor;
mod events;
#[cfg(feature = "hot-reload")]
mod hot_reload;
mod renderer;
pub mod standalone;
mod state;
#[cfg(feature = "softbuffer-blit")]
mod wgpu_offscreen;
mod wgpu_state;
mod window;
#[cfg(feature = "softbuffer-blit")]
mod window_softbuffer;

pub use context::{use_param, use_param_context, use_param_normalized, ParamContext};
#[cfg(not(feature = "softbuffer-blit"))]
pub use standalone::open_parented_x11;
pub use standalone::{
    launch_native_app, open_standalone, open_standalone_with_state, render_screenshot,
};
pub use state::DioxusState;

/// Compiled Tailwind CSS with shadcn/lumen-blocks theme variables.
///
/// This CSS is compiled at build time from `tailwind.css` using the Tailwind v4 CLI.
/// It includes:
/// - Full Tailwind v4 utility classes (tree-shaken to only used classes)
/// - CSS custom properties for light/dark themes (shadcn-compatible)
/// - Theme configuration for lumen-blocks components
///
/// To use this CSS, inject it via a document::Style element in your app component:
/// ```ignore
/// use nice_plug_dioxus::TAILWIND_CSS;
///
/// fn App() -> Element {
///     rsx! {
///         document::Style { {TAILWIND_CSS} }
///         div { class: "dark bg-background text-foreground",
///             // Your app content...
///         }
///     }
/// }
/// ```
///
/// For dark mode, add the `dark` class to a parent element.
pub const TAILWIND_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/tailwind.compiled.css"));

/// Alias for TAILWIND_CSS (legacy name)
pub const THEME_CSS: &str = TAILWIND_CSS;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::context::{use_param, use_param_context, use_param_normalized, ParamContext};
    pub use crate::create_dioxus_editor;
    pub use crate::create_dioxus_editor_with_state;
    pub use crate::custom_paint::{
        use_scene_overlay, use_scene_overlay_background, use_scene_overlay_on_layer, vello,
        OverlayHandle, OverlayLayer, OverlayRegistry, SceneOverlay,
    };
    #[cfg(feature = "embedded")]
    pub use crate::embedded::DioxusEmbeddedEditor;
    pub use crate::state::DioxusState;
    pub use crate::widgets::*;
    pub use crate::SharedState;
    pub use crate::TAILWIND_CSS;
    pub use crate::THEME_CSS;
    pub use dioxus_native::prelude::*;
}

/// Create a Dioxus-based editor for a nice-plug plugin.
///
/// # Arguments
///
/// * `state` - The editor state, which tracks window size and open status
/// * `app` - The Dioxus component function that renders the UI (must be `fn() -> Element`)
///
/// # Example
///
/// ```ignore
/// use nice_plug_dioxus::prelude::*;
///
/// fn create_editor(params: Arc<MyParams>, state: Arc<DioxusState>) -> Option<Box<dyn Editor>> {
///     create_dioxus_editor(state, App)
/// }
///
/// #[component]
/// fn App() -> Element {
///     rsx! {
///         div {
///             h1 { "My Plugin" }
///         }
///     }
/// }
/// ```
///
/// # Note
///
/// The app component must be a function pointer (`fn() -> Element`), not a closure.
/// This is a limitation of Dioxus's VirtualDom. To pass data to your component,
/// use Dioxus contexts (via `provide_context` and `use_context`).
///
/// For plugin parameters, use the `use_param_context()` hook which is automatically
/// provided to all components.
pub fn create_dioxus_editor(
    state: Arc<DioxusState>,
    app: fn() -> Element,
) -> Option<Box<dyn Editor>> {
    Some(Box::new(editor::DioxusEditor::new(state, app)))
}

/// Create a Dioxus-based editor with shared state for a nice-plug plugin.
///
/// This allows the windowed editor to share state with the embedded editor.
/// The shared state will be available via `use_context::<Arc<T>>()` in components.
///
/// # Arguments
///
/// * `state` - The editor state, which tracks window size and open status
/// * `shared_state` - Shared state to inject into the Dioxus context
/// * `app` - The Dioxus component function that renders the UI (must be `fn() -> Element`)
///
/// # Example
///
/// ```ignore
/// use nice_plug_dioxus::prelude::*;
/// use std::sync::atomic::{AtomicI32, Ordering};
///
/// #[derive(Clone)]
/// pub struct SharedUiState {
///     pub counter: Arc<AtomicI32>,
/// }
///
/// fn create_editor(
///     params: Arc<MyParams>,
///     state: Arc<DioxusState>,
///     ui_state: Arc<SharedUiState>,
/// ) -> Option<Box<dyn Editor>> {
///     create_dioxus_editor_with_state(state, ui_state, App)
/// }
///
/// #[component]
/// fn App() -> Element {
///     let ui_state = use_context::<Arc<SharedUiState>>();
///     let counter = ui_state.counter.load(Ordering::Relaxed);
///     
///     rsx! {
///         div {
///             h1 { "Counter: {counter}" }
///         }
///     }
/// }
/// ```
pub fn create_dioxus_editor_with_state<T: std::any::Any + Send + Sync + 'static>(
    state: Arc<DioxusState>,
    shared_state: Arc<T>,
    app: fn() -> Element,
) -> Option<Box<dyn Editor>> {
    let wrapped = SharedState::new(shared_state);
    Some(Box::new(editor::DioxusEditor::new_with_state(
        state, wrapped, app,
    )))
}
