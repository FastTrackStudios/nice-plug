//! Parameter context for Dioxus components.

use dioxus_native::prelude::*;
use nice_plug_core::context::gui::GuiContext;
use nice_plug_core::params::internals::ParamPtr;
use nice_plug_core::params::Param;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Context for interacting with plugin parameters from Dioxus components.
///
/// This is automatically provided to all Dioxus components in a nice_plug_dioxus editor.
/// Use `use_param_context()` to access it.
///
/// # Example
///
/// ```ignore
/// #[component]
/// fn MySlider(param: &'static FloatParam) -> Element {
///     let ctx = use_param_context();
///     let value = use_param(param);
///
///     rsx! {
///         input {
///             r#type: "range",
///             value: "{value}",
///             oninput: move |evt| {
///                 if let Ok(v) = evt.value().parse::<f32>() {
///                     ctx.begin_set(param);
///                     ctx.set(param, v);
///                     ctx.end_set(param);
///                 }
///             }
///         }
///     }
/// }
/// ```
#[derive(Clone)]
pub struct ParamContext {
    gui_context: Arc<dyn GuiContext>,
    needs_redraw: Arc<AtomicBool>,
}

impl ParamContext {
    /// Create a new parameter context.
    pub fn new(gui_context: Arc<dyn GuiContext>, needs_redraw: Arc<AtomicBool>) -> Self {
        Self {
            gui_context,
            needs_redraw,
        }
    }

    /// Begin an automation gesture for a parameter.
    ///
    /// This should be called before modifying a parameter (e.g., on mouse down).
    /// The host will use this to group parameter changes for undo/redo.
    pub fn begin_set<P: Param>(&self, param: &P) {
        unsafe { self.gui_context.raw_begin_set_parameter(param.as_ptr()) }
    }

    /// Set a parameter to a new value.
    ///
    /// This should be called between `begin_set` and `end_set`.
    pub fn set<P: Param>(&self, param: &P, value: P::Plain) {
        let normalized = param.preview_normalized(value);
        unsafe {
            self.gui_context
                .raw_set_parameter_normalized(param.as_ptr(), normalized)
        }
    }

    /// Set a parameter to a normalized value (0.0 - 1.0).
    ///
    /// This should be called between `begin_set` and `end_set`.
    pub fn set_normalized<P: Param>(&self, param: &P, normalized: f32) {
        unsafe {
            self.gui_context
                .raw_set_parameter_normalized(param.as_ptr(), normalized)
        }
    }

    /// End an automation gesture for a parameter.
    ///
    /// This should be called after modifying a parameter (e.g., on mouse up).
    pub fn end_set<P: Param>(&self, param: &P) {
        unsafe { self.gui_context.raw_end_set_parameter(param.as_ptr()) }
    }

    /// Begin an automation gesture for a parameter using a raw pointer.
    ///
    /// This is useful when you only have a `ParamPtr` instead of a `&Param`.
    pub fn begin_set_raw(&self, param: ParamPtr) {
        unsafe { self.gui_context.raw_begin_set_parameter(param) }
    }

    /// Set a parameter to a normalized value using a raw pointer.
    ///
    /// This is useful when you only have a `ParamPtr` instead of a `&Param`.
    pub fn set_normalized_raw(&self, param: ParamPtr, normalized: f32) {
        unsafe {
            self.gui_context
                .raw_set_parameter_normalized(param, normalized)
        }
    }

    /// End an automation gesture for a parameter using a raw pointer.
    ///
    /// This is useful when you only have a `ParamPtr` instead of a `&Param`.
    pub fn end_set_raw(&self, param: ParamPtr) {
        unsafe { self.gui_context.raw_end_set_parameter(param) }
    }

    /// Request a redraw of the UI.
    pub fn request_redraw(&self) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }

    /// Get the underlying GUI context for advanced operations.
    ///
    /// This is primarily useful for saving/restoring plugin state.
    pub fn gui_context(&self) -> &Arc<dyn GuiContext> {
        &self.gui_context
    }

    /// Request the host to rescan parameter info (names, module paths, visibility).
    ///
    /// Call this after changing parameter display names or hiding/showing parameters.
    /// The host will re-query `get_info()` for all parameters without interrupting audio.
    /// Corresponds to `CLAP_PARAM_RESCAN_INFO`.
    pub fn rescan_param_info(&self) {
        self.gui_context.rescan_param_info();
    }

    /// Request the host to fully rescan all parameters, including structural changes.
    ///
    /// Call this after changing parameter ranges, step counts, or adding/removing parameters.
    /// This triggers a plugin restart cycle (deactivate → rescan → activate).
    /// Corresponds to `CLAP_PARAM_RESCAN_ALL`.
    pub fn rescan_param_all(&self) {
        self.gui_context.rescan_param_all();
    }
}

/// Hook to get the parameter context.
///
/// This must be called from within a Dioxus component rendered by `create_dioxus_editor`.
///
/// # Example
///
/// ```ignore
/// #[component]
/// fn MyComponent() -> Element {
///     let ctx = use_param_context();
///     // Use ctx to interact with parameters
///     rsx! { div { "Hello" } }
/// }
/// ```
pub fn use_param_context() -> ParamContext {
    use_context::<ParamContext>()
}

/// Hook to read a parameter's current plain value.
///
/// This reads the parameter's modulated value (after any host modulation is applied).
/// The value is re-read on each render when parameter change notifications trigger a redraw.
///
/// # Example
///
/// ```ignore
/// #[component]
/// fn GainDisplay(param: &'static FloatParam) -> Element {
///     let gain = use_param(param);
///     rsx! {
///         div { "Gain: {gain:.2} dB" }
///     }
/// }
/// ```
pub fn use_param<P: Param>(param: &P) -> P::Plain {
    param.modulated_plain_value()
}

/// Hook to read a parameter's normalized value (0.0 - 1.0).
///
/// This is useful for rendering sliders and other UI elements that work with normalized values.
///
/// # Example
///
/// ```ignore
/// #[component]
/// fn Slider(param: &'static FloatParam) -> Element {
///     let normalized = use_param_normalized(param);
///     rsx! {
///         div {
///             class: "slider-fill",
///             style: "width: {normalized * 100.0}%"
///         }
///     }
/// }
/// ```
pub fn use_param_normalized<P: Param>(param: &P) -> f32 {
    param.modulated_normalized_value()
}
