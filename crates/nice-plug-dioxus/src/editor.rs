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
    Editor, EditorHandle, HostMethods, ParentWindowHandle, ResizeHint, SpawnedEditor,
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
    /// The host telling us which track this instance is on. Upstream pushes
    /// this rather than exposing it on `GuiContext`, so it is stashed on the
    /// shared state where the UI can read it on its next tick.
    fn track_info_updated(&self, info: nice_plug_core::plugin::TrackInfo) {
        self.state.set_track_info(info);
    }

    type Handle = DioxusEditorHandle;

    fn spawn(
        &self,
        parent: Option<ParentWindowHandle>,
        wait_for_parent: bool,
        suggested_scale_factor: Option<f64>,
        gui_context: GuiContext,
        host: Option<HostMethods>,
    ) -> Result<SpawnedEditor<Self::Handle>, Box<dyn Error>> {
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
        // The window thread's route to main-thread time. On X11 baseview parks
        // host callbacks until someone polls them from the main thread, and
        // nothing polls them unless the host is asked for a callback — so
        // without this a plugin-driven resize resizes the child window and the
        // DAW's frame never moves.
        struct MainThreadAdapter {
            caller: Box<dyn nice_plug_core::editor::HostMainThreadCaller>,
        }
        impl baseview::host::HostMainThreadCaller for MainThreadAdapter {
            fn call_main_thread(&mut self) {
                self.caller.call_main_thread();
            }
        }

        // Upstream hands both halves over as `HostMethods` now; the
        // main-thread caller is no longer an optional hook on the callbacks.
        let host = host.map(|HostMethods { callbacks, main_thread_caller }| {
            baseview::host::Host::new()
                .with_callbacks(HostAdapter { host: callbacks })
                .with_main_thread(MainThreadAdapter { caller: main_thread_caller })
        });

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

        Ok(SpawnedEditor {
            handle: DioxusEditorHandle {
                state: self.state.clone(),
                needs_redraw: self.needs_redraw.clone(),
            },
            window,
        })
    }

    /// The editor size in the unit the platform's GUI API uses.
    ///
    /// The return type says physical, and on X11 and Win32 it is. macOS is the
    /// exception: cocoa — and therefore CLAP and VST3 on macOS — is defined in
    /// LOGICAL pixels, so scaling up here reports a Retina host a window twice
    /// as large in each dimension as the editor wants, and the editor then
    /// paints its real size into a quarter of the window it is given.
    ///
    /// The size is stored logical either way; the only question is whether to
    /// scale it on the way out.
    fn size(&self) -> PhysicalSize<u32> {
        let (width, height) = self.state.scaled_logical_size();
        if cfg!(target_os = "macos") {
            return PhysicalSize::new(width, height);
        }
        let scale = self.scaling_factor.load().unwrap_or(1.0) as f64;
        LogicalSize::new(width as f64, height as f64).to_physical(scale)
    }

    fn resize_hint(&self) -> ResizeHint {
        self.state.resize_hint()
    }
}

/// How many frames to re-present for after the host moves or maps the window.
///
/// A tenth of a second at 60fps: long enough to cover a host that maps its
/// frame a few frames after parenting, short enough that nobody can measure
/// the cost.
const REVALIDATE_FRAMES: u32 = 6;

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
        let result = window.set_parent(&RwhAdapter(parent));
        // The window just moved into the host's frame — see
        // `DioxusState::revalidate`.
        self.state.revalidate(REVALIDATE_FRAMES);
        result
    }

    fn show(&self, window: &Self::Window) -> Result<(), Self::Error> {
        // And the host is about to map it, which is the other half of the same
        // problem — see `DioxusState::revalidate`.
        self.state.revalidate(REVALIDATE_FRAMES);
        window.show()
    }

    /// Hand the window thread's queued host callbacks to the host.
    ///
    /// A no-op everywhere but X11, where it is the last link in the resize
    /// chain: editor asks its window to resize → baseview queues a host
    /// callback → the wrapper asks the host for main-thread time → this runs
    /// the callback → the DAW resizes the plugin view.
    fn poll_host_callbacks(&self, window: &mut Self::Window) {
        window.host_main_thread_callback();
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
    fn set_size(
        &self,
        new_size: PhysicalSize<u32>,
        window: &Self::Window,
    ) -> Result<(), Self::Error> {
        let current = window.size();

        // Mirror of `size()`: the host speaks the platform GUI API's unit, so
        // on macOS these numbers are already LOGICAL and converting them again
        // halves the editor on a 2x display. Everywhere else they are physical.
        let (physical, logical) = if cfg!(target_os = "macos") {
            let logical = LogicalSize::new(new_size.width as f64, new_size.height as f64);
            (logical.to_physical(current.scale_factor), logical)
        } else {
            (new_size, new_size.to_logical(current.scale_factor))
        };

        if !self.state.resize_hint().is_size_valid(
            physical,
            current.physical,
            current.scale_factor,
        ) {
            // `baseview::Error` is opaque; a handler error is the documented
            // way to construct one.
            return Err(baseview::HandlerError::from_boxed(
                "requested size is outside the editor's resize hint".into(),
            )
            .into());
        }

        self.state
            .host_set_size(logical.width as u32, logical.height as u32);

        // baseview wants the size in its own terms; hand it the logical size
        // directly rather than a physical one it would have to convert back.
        if let Err(e) = window.resize(LogicalSize::new(logical.width, logical.height)) {
            nice_plug_core::nice_error!("Failed to resize editor window to {logical:?}: {e}");
            return Err(e);
        }
        self.needs_redraw.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// The host giving the window thread main-thread time it asked for. On X11
    /// baseview parks host callbacks until they are polled from the main
    /// thread; this is that poll, and without it a plugin-driven resize moves
    /// the child window and never the DAW's frame.
    fn host_main_thread_callback(&self, window: &Self::Window) {
        window.host_main_thread_callback();
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

    fn set_fallback_scale_factor(
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
