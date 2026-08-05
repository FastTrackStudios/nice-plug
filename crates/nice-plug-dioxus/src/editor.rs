//! The [`Editor`] trait implementation for Dioxus editors.

use crate::state::DioxusState;
#[cfg(not(feature = "softbuffer-blit"))]
use crate::window::DioxusWindowHandler;
#[cfg(feature = "softbuffer-blit")]
use crate::window_softbuffer::DioxusSoftbufferWindowHandler;
use crate::SharedState;
use crossbeam::atomic::AtomicCell;
use dioxus_native::prelude::Element;
use nice_plug_core::context::gui::GuiContext;
use nice_plug_core::editor::dpi::{LogicalSize, PhysicalSize};
use nice_plug_core::editor::{
    Editor, EditorHandle, EditorWindow, ParentWindowHandle, ResizeHint,
};
use std::error::Error;
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
    type Handle = DioxusEditorHandle;

    fn spawn(
        &self,
        parent: Option<ParentWindowHandle>,
        wait_for_parent: bool,
        suggested_scale_factor: Option<f64>,
        gui_context: GuiContext,
        host: Option<Box<dyn nice_plug_core::editor::HostCallbacks>>,
    ) -> Result<EditorWindow<Self::Handle>, Box<dyn Error>> {
        let (width, height) = self.state.inner_logical_size();
        let scaling_factor = self.scaling_factor.load();

        let app = self.app;
        let dioxus_state = self.state.clone();
        let needs_redraw = self.needs_redraw.clone();
        let shared_state = self.shared_state.clone();

        // `nice-plug-core` mirrors `baseview::host::HostCallbacks` rather than
        // depending on baseview directly (baseview has no stable release yet),
        // so bridge the two. This is the channel a resize request travels: the
        // editor asks its baseview window to resize, baseview asks the host
        // through here, and the DAW resizes the plugin view.
        struct HostAdapter {
            host: Box<dyn nice_plug_core::editor::HostCallbacks>,
        }
        impl baseview::host::HostCallbacks for HostAdapter {
            fn request_resize(
                &mut self,
                new_size: baseview::WindowSize,
            ) -> Result<(), baseview::HandlerError> {
                self.host
                    .request_resize(new_size.physical.into(), new_size.scale_factor)
                    .map_err(baseview::HandlerError::from_boxed)
            }

            fn destroyed(&mut self) {
                self.host.destroyed();
            }
        }
        let host = host.map(|host| baseview::host::Host::new().with_callbacks(HostAdapter { host }));

        // `with_parent` borrows the adapter, so it has to outlive the builder.
        let parent_adapter = parent.map(RwhAdapter);
        let mut settings = baseview::WindowSettings::new()
            .with_title("Plugin Editor")
            .with_size(baseview::dpi::LogicalSize::new(width as f64, height as f64))
            .with_wait_for_parent(wait_for_parent)
            .with_fallback_scale_factor(scaling_factor.map(|f| f as f64));
        if let Some(parent_adapter) = parent_adapter.as_ref() {
            settings = settings.with_parent(parent_adapter);
        }

        let window = baseview::Window::create_with_host(
            settings,
            move |window_context| {
                #[cfg(feature = "softbuffer-blit")]
                {
                    Ok(DioxusSoftbufferWindowHandler::new_with_state(
                        window_context,
                        app,
                        gui_context,
                        dioxus_state,
                        needs_redraw,
                        shared_state,
                    ))
                }
                #[cfg(not(feature = "softbuffer-blit"))]
                {
                    Ok(DioxusWindowHandler::new_with_state(
                        window_context,
                        app,
                        gui_context,
                        dioxus_state,
                        needs_redraw,
                        shared_state,
                    ))
                }
            },
            host,
        )?;

        if let Some(scale_factor) = suggested_scale_factor {
            window.suggest_fallback_scale_factor(scale_factor)?;
        }

        self.state.set_open(true);

        Ok(EditorWindow {
            handle: DioxusEditorHandle {
                state: self.state.clone(),
                needs_redraw: self.needs_redraw.clone(),
            },
            window,
        })
    }

    fn size(&self) -> PhysicalSize<u32> {
        let (width, height) = self.state.scaled_logical_size();
        let scale = self.scaling_factor.load().unwrap_or(1.0) as f64;
        LogicalSize::new(width as f64, height as f64).to_physical(scale)
    }

    fn resize_hint(&self) -> ResizeHint {
        self.state.resize_hint()
    }
}

/// Handle to a spawned [`DioxusEditor`].
///
/// Owns nothing window-shaped: baseview 0.3 hands the window back separately in
/// [`EditorWindow`] and closes it when that is dropped, so this only carries the
/// shared state the host-facing callbacks need.
pub struct DioxusEditorHandle {
    state: Arc<DioxusState>,
    needs_redraw: Arc<AtomicBool>,
}

// The handle itself holds only `Arc`s of `Sync` state; baseview's window is
// tracked separately and is what carries the raw pointers.
unsafe impl Send for DioxusEditorHandle {}

impl EditorHandle for DioxusEditorHandle {
    type Window = baseview::Window;
    type Error = baseview::Error;

    fn run_until_closed(window: Self::Window) -> Result<(), Self::Error> {
        window.run_until_closed()
    }

    fn set_parent(
        &self,
        parent: ParentWindowHandle,
        window: &Self::Window,
    ) -> Result<(), Self::Error> {
        window.set_parent(&RwhAdapter(parent))
    }

    fn show(&self, window: &Self::Window) -> Result<(), Self::Error> {
        window.show()
    }

    fn hide(&self, window: &Self::Window) -> Result<(), Self::Error> {
        window.hide()
    }

    /// The host resized the plugin view — either because it accepted an earlier
    /// `request_resize`, or because the user dragged its resize handle.
    ///
    /// Records the new size on [`DioxusState`] as a *host* resize so the window
    /// handler reconfigures the surface and relayouts blitz without asking the
    /// host to resize again (that would loop), then resizes the child window to
    /// match.
    fn set_size(&self, new_size: PhysicalSize<u32>, window: &Self::Window) -> bool {
        let current = window.size();
        if !self.state.resize_hint().is_size_valid(
            new_size,
            current.physical,
            current.scale_factor,
        ) {
            return false;
        }

        let logical: LogicalSize<f64> = new_size.to_logical(current.scale_factor);
        self.state
            .host_set_size(logical.width as u32, logical.height as u32);

        if let Err(e) = window.resize(new_size) {
            nice_plug_core::nice_error!("Failed to resize editor window to {new_size:?}: {e}");
            return false;
        }
        self.needs_redraw.store(true, Ordering::Relaxed);
        true
    }

    fn adjust_size(
        &self,
        new_size: PhysicalSize<u32>,
        window: &Self::Window,
    ) -> Option<PhysicalSize<u32>> {
        let current = window.size();
        Some(self.state.resize_hint().adjust_size(
            new_size,
            current.physical,
            current.scale_factor,
        ))
    }

    fn set_suggested_scale_factor(
        &self,
        scale_factor: f64,
        window: &Self::Window,
    ) -> Result<(), Self::Error> {
        window.suggest_fallback_scale_factor(scale_factor)
    }

    fn state_changed(&self) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {
        self.needs_redraw.store(true, Ordering::Relaxed);
    }
}

impl Drop for DioxusEditorHandle {
    fn drop(&mut self) {
        self.state.set_open(false);
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
        use std::num::NonZeroU32;

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
