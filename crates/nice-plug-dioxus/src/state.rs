//! Editor state management for Dioxus editors.

use crossbeam::atomic::AtomicCell;
use nice_plug_core::params::persist::PersistentField;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// State for a `nice_plug_dioxus` editor.
///
/// This tracks the window size, scale factor, and whether the editor is currently open.
/// The state can be persisted with the plugin's parameters using the `#[persist]` attribute.
///
/// # Example
///
/// ```ignore
/// #[derive(Params)]
/// struct MyParams {
///     #[persist = "editor-state"]
///     editor_state: Arc<DioxusState>,
///
///     #[id = "gain"]
///     gain: FloatParam,
/// }
///
/// impl Default for MyParams {
///     fn default() -> Self {
///         Self {
///             editor_state: DioxusState::new(|| (400, 300)),
///             gain: FloatParam::new("Gain", 1.0, FloatRange::Linear { min: 0.0, max: 2.0 }),
///         }
///     }
/// }
/// ```
#[derive(Serialize, Deserialize)]
pub struct DioxusState {
    /// Default size function (used when size hasn't been explicitly set).
    #[serde(skip, default = "empty_size_fn")]
    default_size_fn: Box<dyn Fn() -> (u32, u32) + Send + Sync>,

    /// Current window width (0 means use default_size_fn).
    #[serde(with = "nice_plug_core::params::persist::serialize_atomic_cell")]
    width: AtomicCell<u32>,

    /// Current window height (0 means use default_size_fn).
    #[serde(with = "nice_plug_core::params::persist::serialize_atomic_cell")]
    height: AtomicCell<u32>,

    /// A scale factor applied on top of any system HiDPI scaling.
    #[serde(with = "nice_plug_core::params::persist::serialize_atomic_cell")]
    scale_factor: AtomicCell<f64>,

    /// Whether the editor window is currently open.
    #[serde(skip)]
    open: AtomicBool,

    /// Pending resize request (width, height). Set by UI, consumed by window handler.
    /// This triggers window.resize() + gui_context.request_resize() (plugin→host).
    #[serde(skip)]
    pending_resize: AtomicCell<Option<(u32, u32)>>,

    /// Pending host-driven resize (width, height). Set by host via set_size(),
    /// consumed by window handler. Only updates viewport/wgpu, does NOT call
    /// back to the host (avoiding feedback loop).
    #[serde(skip)]
    pending_host_resize: AtomicCell<Option<(u32, u32)>>,
}

fn empty_size_fn() -> Box<dyn Fn() -> (u32, u32) + Send + Sync> {
    Box::new(|| (0, 0))
}

impl Debug for DioxusState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (width, height) = self.size();
        f.debug_struct("DioxusState")
            .field("size", &format!("({}, {})", width, height))
            .field("scale_factor", &self.scale_factor)
            .field("open", &self.open)
            .finish()
    }
}

impl DioxusState {
    /// Create a new editor state with a default size function.
    ///
    /// The size function provides the initial/default window size in logical pixels.
    /// The window can be resized by the user, and the new size will be persisted.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Static default size
    /// let state = DioxusState::new(|| (400, 300));
    /// ```
    pub fn new(default_size_fn: impl Fn() -> (u32, u32) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            default_size_fn: Box::new(default_size_fn),
            width: AtomicCell::new(0),
            height: AtomicCell::new(0),
            scale_factor: AtomicCell::new(1.0),
            open: AtomicBool::new(false),
            pending_resize: AtomicCell::new(None),
            pending_host_resize: AtomicCell::new(None),
        })
    }

    /// Create a new editor state with a custom default scale factor.
    ///
    /// This scale factor is applied on top of any system HiDPI scaling.
    pub fn new_with_default_scale_factor(
        default_size_fn: impl Fn() -> (u32, u32) + Send + Sync + 'static,
        default_scale_factor: f64,
    ) -> Arc<Self> {
        Arc::new(Self {
            default_size_fn: Box::new(default_size_fn),
            width: AtomicCell::new(0),
            height: AtomicCell::new(0),
            scale_factor: AtomicCell::new(default_scale_factor),
            open: AtomicBool::new(false),
            pending_resize: AtomicCell::new(None),
            pending_host_resize: AtomicCell::new(None),
        })
    }

    /// Returns the current window size in logical pixels.
    /// If the size hasn't been explicitly set, returns the default size.
    pub fn size(&self) -> (u32, u32) {
        let w = self.width.load();
        let h = self.height.load();
        if w == 0 || h == 0 {
            (self.default_size_fn)()
        } else {
            (w, h)
        }
    }

    /// Set the window size. This updates the stored size but doesn't resize the window directly.
    /// Call `request_resize` to actually resize the window.
    pub fn set_size(&self, width: u32, height: u32) {
        self.width.store(width);
        self.height.store(height);
    }

    /// Request a window resize. The window handler will pick this up and resize the window.
    /// Note: This overwrites any pending resize, so rapid calls will only process the latest.
    pub fn request_resize(&self, width: u32, height: u32) {
        nice_plug_core::nice_log!("[STATE] request_resize called: {}x{}", width, height);
        // Only store if size actually changed to reduce unnecessary updates
        let current = self.pending_resize.load();
        if current != Some((width, height)) {
            self.pending_resize.store(Some((width, height)));
            nice_plug_core::nice_log!("[STATE] Stored pending resize: {}x{}", width, height);
        }
    }

    /// Take the pending resize request, if any. Used by the window handler.
    pub fn take_pending_resize(&self) -> Option<(u32, u32)> {
        let result = self.pending_resize.take();
        if result.is_some() {
            nice_plug_core::nice_log!("[STATE] take_pending_resize: {:?}", result);
        }
        result
    }

    /// Queue a host-driven resize. The window handler updates viewport/wgpu
    /// but does NOT call window.resize() or gui_context.request_resize().
    pub fn host_set_size(&self, width: u32, height: u32) {
        self.pending_host_resize.store(Some((width, height)));
    }

    /// Take the pending host-driven resize, if any.
    pub fn take_pending_host_resize(&self) -> Option<(u32, u32)> {
        self.pending_host_resize.take()
    }

    /// Returns the window size in logical pixels after applying the user scale factor.
    pub fn scaled_logical_size(&self) -> (u32, u32) {
        let (width, height) = self.size();
        let scale = self.scale_factor.load();
        (
            (width as f64 * scale).round() as u32,
            (height as f64 * scale).round() as u32,
        )
    }

    /// Returns the window size in logical pixels before applying the user scale factor.
    /// Alias for `size()` for backwards compatibility.
    pub fn inner_logical_size(&self) -> (u32, u32) {
        self.size()
    }

    /// Get the user scale factor.
    pub fn user_scale_factor(&self) -> f64 {
        self.scale_factor.load()
    }

    /// Set the user scale factor.
    pub fn set_user_scale_factor(&self, factor: f64) {
        self.scale_factor.store(factor);
    }

    /// Returns whether the editor window is currently open.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// Set the open state (internal use).
    pub(crate) fn set_open(&self, open: bool) {
        self.open.store(open, Ordering::Release);
    }
}

impl<'a> PersistentField<'a, DioxusState> for Arc<DioxusState> {
    fn set(&self, new_value: DioxusState) {
        self.width.store(new_value.width.load());
        self.height.store(new_value.height.load());
        self.scale_factor.store(new_value.scale_factor.load());
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&DioxusState) -> R,
    {
        f(self)
    }
}
