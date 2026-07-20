//! The [`Editor`] trait implementation for Dioxus editors.

use crate::state::DioxusState;
#[cfg(not(feature = "softbuffer-blit"))]
use crate::window::DioxusWindowHandler;
#[cfg(feature = "softbuffer-blit")]
use crate::window_softbuffer::DioxusSoftbufferWindowHandler;
use crate::SharedState;
use baseview::{Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use crossbeam::atomic::AtomicCell;
use dioxus_native::prelude::Element;
use nice_plug_core::context::gui::GuiContext;
use nice_plug_core::editor::{Editor, ParentWindowHandle};
use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// An [`Editor`] implementation that renders a Dioxus UI using the Blitz/Vello renderer.
pub struct DioxusEditor {
    pub(crate) state: Arc<DioxusState>,
    pub(crate) app: fn() -> Element,
    pub(crate) scaling_factor: AtomicCell<Option<f32>>,
    pub(crate) needs_redraw: Arc<AtomicBool>,
    /// Optional shared state to inject into Dioxus context.
    /// This allows windowed and embedded editors to share state.
    pub(crate) shared_state: Option<SharedState>,
}

impl DioxusEditor {
    pub fn new(state: Arc<DioxusState>, app: fn() -> Element) -> Self {
        Self {
            state,
            app,
            // On macOS, we use the system scaling factor
            #[cfg(target_os = "macos")]
            scaling_factor: AtomicCell::new(None),
            #[cfg(not(target_os = "macos"))]
            scaling_factor: AtomicCell::new(Some(1.0)),
            needs_redraw: Arc::new(AtomicBool::new(false)),
            shared_state: None,
        }
    }

    /// Create a new editor with shared state that will be injected into Dioxus context.
    ///
    /// The shared state will be available via `use_context::<SharedState>()` in components,
    /// which can then be downcast to your concrete type using `shared_state.get::<T>()`.
    pub fn new_with_state(
        state: Arc<DioxusState>,
        shared_state: SharedState,
        app: fn() -> Element,
    ) -> Self {
        Self {
            state,
            app,
            #[cfg(target_os = "macos")]
            scaling_factor: AtomicCell::new(None),
            #[cfg(not(target_os = "macos"))]
            scaling_factor: AtomicCell::new(Some(1.0)),
            needs_redraw: Arc::new(AtomicBool::new(false)),
            shared_state: Some(shared_state),
        }
    }
}

impl Editor for DioxusEditor {
    fn spawn(
        &self,
        parent: ParentWindowHandle,
        context: Arc<dyn GuiContext>,
    ) -> Box<dyn Any> {
        let (width, height) = self.state.inner_logical_size();
        let scaling_factor = self.scaling_factor.load();

        let app = self.app;
        let gui_context = context.clone();
        let dioxus_state = self.state.clone();
        let needs_redraw = self.needs_redraw.clone();
        let shared_state = self.shared_state.clone();

        let window = baseview::Window::open_parented(
            &RwhAdapter(parent),
            WindowOpenOptions {
                title: String::from("Plugin Editor"),
                size: Size::new(width as f64, height as f64),
                scale: scaling_factor
                    .map(|f| WindowScalePolicy::ScaleFactor(f as f64))
                    .unwrap_or(WindowScalePolicy::SystemScaleFactor),
            },
            move |window| {
                #[cfg(feature = "softbuffer-blit")]
                {
                    DioxusSoftbufferWindowHandler::new_with_state(
                        window,
                        app,
                        gui_context.clone(),
                        dioxus_state.clone(),
                        needs_redraw.clone(),
                        shared_state,
                    )
                }
                #[cfg(not(feature = "softbuffer-blit"))]
                {
                    DioxusWindowHandler::new_with_state(
                        window,
                        app,
                        gui_context.clone(),
                        dioxus_state.clone(),
                        needs_redraw.clone(),
                        shared_state,
                    )
                }
            },
        );

        self.state.set_open(true);
        Box::new(DioxusEditorHandle {
            state: self.state.clone(),
            window,
        })
    }

    fn size(&self) -> nice_plug_core::editor::dpi::Size {
        let (width, height) = self.state.scaled_logical_size();
        nice_plug_core::editor::dpi::LogicalSize::new(width, height).into()
    }

    fn set_scale_factor(&self, factor: f64) -> bool {
        // Don't allow scale factor changes while the editor is open
        if self.state.is_open() {
            return false;
        }
        self.scaling_factor.store(Some(factor as f32));
        true
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }

    fn param_values_changed(&self) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }
}

/// Handle returned from `Editor::spawn()` that closes the window when dropped.
struct DioxusEditorHandle {
    state: Arc<DioxusState>,
    window: WindowHandle,
}

// The window handle contains raw pointers
unsafe impl Send for DioxusEditorHandle {}

impl Drop for DioxusEditorHandle {
    fn drop(&mut self) {
        self.state.set_open(false);
        self.window.close();
    }
}

/// Adapter to convert nice-plug's `ParentWindowHandle` to raw-window-handle 0.6 traits
/// (which is what baseview expects with the upgrade_rwh branch).
///
/// `nice_plug_core` links raw-window-handle 0.5, but blitz/wgpu need 0.6, so we
/// read the enum variants directly and build 0.6 handles rather than going through
/// `ParentWindowHandle`'s own `HasRawWindowHandle` (0.5) impl.
struct RwhAdapter(ParentWindowHandle);

impl raw_window_handle::HasWindowHandle for RwhAdapter {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        use raw_window_handle::RawWindowHandle;
        use std::num::{NonZeroIsize, NonZeroU32};
        use std::ptr::NonNull;

        let raw = match self.0 {
            ParentWindowHandle::XlibWindow(window) => {
                // Present the Xlib window ID as an XCB handle: blitz/wgpu/softbuffer
                // connect over XCB, and X11 window IDs are shared between the two.
                let handle = raw_window_handle::XcbWindowHandle::new(
                    NonZeroU32::new(window as u32).expect("X11 window ID should not be 0"),
                );
                RawWindowHandle::Xcb(handle)
            }
            ParentWindowHandle::XcbWindow(window) => {
                RawWindowHandle::Xcb(raw_window_handle::XcbWindowHandle::new(window))
            }
            ParentWindowHandle::AppKitNsView(ns_view) => {
                let handle = raw_window_handle::AppKitWindowHandle::new(ns_view);
                RawWindowHandle::AppKit(handle)
            }
            ParentWindowHandle::Win32Hwnd(hwnd) => {
                let handle = raw_window_handle::Win32WindowHandle::new(hwnd);
                RawWindowHandle::Win32(handle)
            }
        };
        // Safety: The handle is valid for the lifetime of the adapter
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

impl raw_window_handle::HasDisplayHandle for RwhAdapter {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        use raw_window_handle::RawDisplayHandle;

        let raw = match self.0 {
            ParentWindowHandle::XlibWindow(_) | ParentWindowHandle::XcbWindow(_) => {
                // For X11, we need a display connection, but we don't have one
                // from the parent handle. Use an empty XCB display handle.
                let handle = raw_window_handle::XcbDisplayHandle::new(None, 0);
                RawDisplayHandle::Xcb(handle)
            }
            ParentWindowHandle::AppKitNsView(_) => {
                let handle = raw_window_handle::AppKitDisplayHandle::new();
                RawDisplayHandle::AppKit(handle)
            }
            ParentWindowHandle::Win32Hwnd(_) => {
                let handle = raw_window_handle::WindowsDisplayHandle::new();
                RawDisplayHandle::Windows(handle)
            }
        };
        // Safety: The handle is valid for the lifetime of the adapter
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw) })
    }
}
