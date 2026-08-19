//! Baseview window handler for Dioxus editors.
//!
//! This module provides the standard wgpu surface-based window handler.
//! For Linux/XWayland compatibility, use the `softbuffer-blit` feature which
//! renders with wgpu to an offscreen texture and blits via softbuffer.

// This module is only used when softbuffer-blit is NOT enabled
#![cfg(not(feature = "softbuffer-blit"))]

use crate::context::ParamContext;
use crate::events::translate_event;
use crate::renderer::Renderer;
use crate::state::DioxusState;
use crate::wgpu_state::WgpuState;
use crate::SharedState;

#[cfg(feature = "hot-reload")]
use crate::hot_reload::HotReloadState;

use baseview::{Event, EventStatus, HandlerError, MouseCursor, WindowContext, WindowHandler};
use baseview::dpi::LogicalSize;
use std::cell::RefCell;
use blitz_dom::{Document as _, DocumentConfig};
use blitz_traits::events::MouseEventButtons;
use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport};
use crossbeam::channel::{unbounded, Receiver, Sender};
use cursor_icon::CursorIcon;
use dioxus_native::prelude::*;
use dioxus_native::DioxusDocument;
use futures_util::task::ArcWake;
use nice_plug_core::context::gui::GuiContext;

// Use Modifiers from our events module which handles the version conflict
use crate::events::Modifiers;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

/// Messages sent from Dioxus components to the window handler.
/// Used for document operations like injecting stylesheets.
enum DocumentMessage {
    CreateHeadElement {
        name: String,
        attributes: Vec<(String, String)>,
        contents: Option<String>,
    },
}

/// Proxy for document operations from Dioxus components.
/// Implements `dioxus::document::Document` to enable `document::Style` etc.
#[derive(Clone)]
pub struct DocumentProxy {
    sender: Sender<DocumentMessage>,
}

impl DocumentProxy {
    fn new(sender: Sender<DocumentMessage>) -> Self {
        Self { sender }
    }

    fn create_head_element(
        &self,
        name: &str,
        attributes: &[(&str, String)],
        contents: Option<String>,
    ) {
        let _ = self.sender.send(DocumentMessage::CreateHeadElement {
            name: name.to_string(),
            attributes: attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            contents,
        });
    }
}

fn diagnostics_enabled() -> bool {
    std::env::var_os("NIH_DIOXUS_DIAGNOSTICS").is_some()
}

impl document::Document for DocumentProxy {
    fn eval(&self, js: String) -> document::Eval {
        // No-op for native - we don't support JS eval
        document::NoOpDocument.eval(js)
    }

    fn set_title(&self, title: String) {
        self.create_head_element("title", &[], Some(title));
    }

    fn create_meta(&self, props: document::MetaProps) {
        self.create_head_element("meta", &props.attributes(), None);
    }

    fn create_script(&self, props: document::ScriptProps) {
        self.create_head_element("script", &props.attributes(), props.script_contents().ok());
    }

    fn create_style(&self, props: document::StyleProps) {
        self.create_head_element("style", &props.attributes(), props.style_contents().ok());
    }

    fn create_link(&self, props: document::LinkProps) {
        self.create_head_element("link", &props.attributes(), None);
    }

    fn create_head_component(&self) -> bool {
        true
    }
}

/// Bridges blitz's ShellProvider callbacks to baseview's Window API.
///
/// Cursor change requests are queued in an Arc<Mutex> and applied by
/// the window handler on the next frame.
struct BaseviewShellProvider {
    cursor: Arc<Mutex<Option<CursorIcon>>>,
}

impl ShellProvider for BaseviewShellProvider {
    fn set_cursor(&self, icon: Option<CursorIcon>) {
        *self.cursor.lock().unwrap() = Some(icon.unwrap_or(CursorIcon::Default));
    }
}

fn cursor_icon_to_baseview(icon: CursorIcon) -> MouseCursor {
    match icon {
        CursorIcon::Default => MouseCursor::Default,
        CursorIcon::Pointer => MouseCursor::Hand,
        CursorIcon::Grab => MouseCursor::Hand,
        CursorIcon::Grabbing => MouseCursor::HandGrabbing,
        CursorIcon::Text => MouseCursor::Text,
        CursorIcon::VerticalText => MouseCursor::VerticalText,
        CursorIcon::Move => MouseCursor::Move,
        CursorIcon::AllScroll => MouseCursor::AllScroll,
        CursorIcon::Crosshair => MouseCursor::Crosshair,
        CursorIcon::Cell => MouseCursor::Cell,
        CursorIcon::Alias => MouseCursor::Alias,
        CursorIcon::Copy => MouseCursor::Copy,
        CursorIcon::NoDrop => MouseCursor::NotAllowed,
        CursorIcon::NotAllowed => MouseCursor::NotAllowed,
        CursorIcon::ZoomIn => MouseCursor::ZoomIn,
        CursorIcon::ZoomOut => MouseCursor::ZoomOut,
        CursorIcon::EResize => MouseCursor::EResize,
        CursorIcon::NResize => MouseCursor::NResize,
        CursorIcon::NeResize => MouseCursor::NeResize,
        CursorIcon::NwResize => MouseCursor::NwResize,
        CursorIcon::SResize => MouseCursor::SResize,
        CursorIcon::SeResize => MouseCursor::SeResize,
        CursorIcon::SwResize => MouseCursor::SwResize,
        CursorIcon::WResize => MouseCursor::WResize,
        CursorIcon::EwResize => MouseCursor::EwResize,
        CursorIcon::NsResize => MouseCursor::NsResize,
        CursorIcon::NeswResize => MouseCursor::NeswResize,
        CursorIcon::NwseResize => MouseCursor::NwseResize,
        CursorIcon::ColResize => MouseCursor::ColResize,
        CursorIcon::RowResize => MouseCursor::RowResize,
        _ => MouseCursor::Default,
    }
}

/// The baseview window handler for Dioxus editors using standard wgpu surface.
///
/// baseview 0.3 calls its handler through `&self` and no longer passes a
/// `&mut Window` in, so the mutable render/input state lives in a `RefCell` and
/// the window is captured once at construction. [`HandlerState`] holds that
/// state and keeps the original `&mut self` method bodies; this type is the
/// `&self` shell baseview actually drives.
pub struct DioxusWindowHandler {
    state: RefCell<HandlerState>,
    window: WindowContext,
}

struct HandlerState {
    // Dioxus state
    dioxus_doc: Option<DioxusDocument>,
    app: fn() -> Element,
    animation_start: Instant,

    // Rendering - standard wgpu surface mode
    wgpu_state: Option<WgpuState>,
    renderer: Option<Renderer>,

    // nice-plug integration
    gui_context: GuiContext,
    dioxus_state: Arc<DioxusState>,
    needs_redraw: Arc<AtomicBool>,

    // Shared UI state (injected into Dioxus context)
    shared_state: Option<SharedState>,

    // Reactive window size signal — written on resize, readable in components
    window_size_signal: Option<Signal<(u32, u32)>>,

    // Document message channel (for document::Style etc.)
    doc_message_receiver: Option<Receiver<DocumentMessage>>,

    // Input state
    mouse_pos: (f32, f32),
    mouse_buttons: MouseEventButtons,
    modifiers: Modifiers,
    // Cursor icon requested by blitz (e.g. pointer over a clickable element)
    pending_cursor: Arc<Mutex<Option<CursorIcon>>>,

    // Hot reload
    #[cfg(feature = "hot-reload")]
    hot_reload: HotReloadState,

    // Window dimensions in PHYSICAL pixels (for wgpu surface and Blitz viewport)
    width: u32,
    height: u32,
    // System scale factor (from resize events)
    scale_factor: f32,
    // Whether we've received a resize event with the actual scale factor
    received_resize: bool,
    /// Frames spent waiting for the view to join a window before giving up and
    /// initializing anyway (see `init_geometry`). Only macOS has to wait.
    #[cfg(target_os = "macos")]
    init_wait_frames: u32,

    // FPS tracking
    fps_frame_count: u32,
    fps_last_report: Instant,
    last_frame_start: Instant,

    // DOM update throttling: only run mark_all_dirty+poll+resolve at ~30fps
    // so the render loop can run at full speed between DOM updates.
    last_dom_update: Instant,

    // Cached window handles for wgpu surface creation (raw-window-handle 0.6 types)
    window_handle: Option<RawWindowHandle>,
    display_handle: Option<RawDisplayHandle>,
}

#[allow(dead_code)] // constructor + timing kept for the standalone shell
impl DioxusWindowHandler {
    /// Create a new window handler.
    ///
    /// The `app` function must be a function pointer (not a closure) because
    /// `VirtualDom::new` requires `fn() -> Element`.
    pub fn new(
        window: WindowContext,
        app: fn() -> Element,
        gui_context: GuiContext,
        dioxus_state: Arc<DioxusState>,
        needs_redraw: Arc<AtomicBool>,
    ) -> Self {
        Self::new_with_state(window, app, gui_context, dioxus_state, needs_redraw, None)
    }

    /// Create a new window handler with shared state.
    ///
    /// The shared state will be injected into the Dioxus context and available
    /// via `use_context::<SharedState>()` in components.
    pub fn new_with_state(
        window: WindowContext,
        app: fn() -> Element,
        gui_context: GuiContext,
        dioxus_state: Arc<DioxusState>,
        needs_redraw: Arc<AtomicBool>,
        shared_state: Option<SharedState>,
    ) -> Self {
        let state =
            HandlerState::new_with_state(&window, app, gui_context, dioxus_state, needs_redraw, shared_state);
        Self {
            state: RefCell::new(state),
            window,
        }
    }
}

#[allow(dead_code)]
impl HandlerState {
    /// Create a new window handler.
    ///
    /// The `app` function must be a function pointer (not a closure) because
    /// VirtualDom::new requires `fn() -> Element`.
    fn new(
        window: &WindowContext,
        app: fn() -> Element,
        gui_context: GuiContext,
        dioxus_state: Arc<DioxusState>,
        needs_redraw: Arc<AtomicBool>,
    ) -> Self {
        Self::new_with_state(window, app, gui_context, dioxus_state, needs_redraw, None)
    }

    /// Create a new window handler with shared state.
    ///
    /// The shared state will be injected into the Dioxus context and available
    /// via `use_context::<SharedState>()` in components.
    fn new_with_state(
        window: &WindowContext,
        app: fn() -> Element,
        gui_context: GuiContext,
        dioxus_state: Arc<DioxusState>,
        needs_redraw: Arc<AtomicBool>,
        shared_state: Option<SharedState>,
    ) -> Self {
        // Get initial logical size from the dioxus state (this is what we asked for)
        let (logical_width, logical_height) = dioxus_state.inner_logical_size();

        // On macOS, we use SystemScaleFactor which means we don't know the actual
        // scale until we get a resize event. Default to 1.0 but this will be updated.
        // We estimate 2.0 for Retina displays as a reasonable starting point.
        #[cfg(target_os = "macos")]
        let scale_factor = 2.0f32; // Retina default
        #[cfg(not(target_os = "macos"))]
        let scale_factor = 1.0f32;

        // Get raw window handles using baseview's raw-window-handle 0.5 API
        // and convert them to raw-window-handle 0.6 types for wgpu
        let (window_handle, display_handle) = get_raw_handles_from_baseview(window);

        // Debug: log what handles we got
        nice_plug_core::nice_log!(
            "[HANDLES] window: {:?}, display: {:?}",
            window_handle.as_ref().map(|h| format!("{:?}", h)),
            display_handle.as_ref().map(|h| format!("{:?}", h))
        );

        // Calculate initial physical size (will be corrected on first resize event)
        let physical_width = (logical_width as f32 * scale_factor) as u32;
        let physical_height = (logical_height as f32 * scale_factor) as u32;

        Self {
            dioxus_doc: None,
            app,
            animation_start: Instant::now(),
            wgpu_state: None,
            renderer: None,
            gui_context,
            dioxus_state,
            needs_redraw,
            shared_state,
            doc_message_receiver: None,
            mouse_pos: (0.0, 0.0),
            mouse_buttons: MouseEventButtons::empty(),
            modifiers: Modifiers::empty(),
            pending_cursor: Arc::new(Mutex::new(None)),
            #[cfg(feature = "hot-reload")]
            hot_reload: HotReloadState::new(),
            // Store PHYSICAL dimensions - updated on resize events
            width: physical_width,
            height: physical_height,
            scale_factor,
            received_resize: false,
            #[cfg(target_os = "macos")]
            init_wait_frames: 0,
            window_handle,
            display_handle,
            fps_frame_count: 0,
            fps_last_report: Instant::now(),
            last_frame_start: Instant::now(),
            last_dom_update: Instant::now(),
            window_size_signal: None,
        }
    }

    /// Initialize the Dioxus document and rendering state.
    fn initialize(&mut self) {
        let (Some(window_handle), Some(display_handle)) = (self.window_handle, self.display_handle)
        else {
            nice_plug_core::nice_error!("Cannot initialize: missing window handles");
            return;
        };

        // self.width and self.height are already in PHYSICAL pixels
        let physical_width = self.width.max(1);
        let physical_height = self.height.max(1);

        nice_plug_core::nice_log!(
            "[INIT] physical: {}x{}, scale: {}",
            physical_width,
            physical_height,
            self.scale_factor
        );

        // Create wgpu state using physical size for the GPU surface
        let wgpu_state = WgpuState::new_from_raw(
            window_handle,
            display_handle,
            physical_width,
            physical_height,
        );

        // Create renderer
        let renderer = Renderer::new(&wgpu_state.device);
        let overlay_registry = renderer.overlay_registry();

        // Create the Dioxus virtual DOM
        let vdom = VirtualDom::new(self.app);

        // Create viewport with PHYSICAL size and scale factor
        let viewport = Viewport::new(
            self.width,
            self.height,
            self.scale_factor,
            ColorScheme::Light,
        );

        nice_plug_core::nice_log!(
            "[VIEWPORT] Creating DioxusDocument with viewport: {}x{} physical, scale={}",
            self.width,
            self.height,
            self.scale_factor
        );

        // Create the Dioxus document
        let mut dioxus_doc = DioxusDocument::new(
            vdom,
            DocumentConfig {
                viewport: Some(viewport),
                ..Default::default()
            },
        );

        // Connect the blitz ShellProvider so cursor changes from layout/hover
        // are forwarded to the baseview window.
        let shell_provider = BaseviewShellProvider {
            cursor: self.pending_cursor.clone(),
        };
        dioxus_doc
            .inner_mut()
            .set_shell_provider(Arc::new(shell_provider));

        // Create channel for document messages (for document::Style etc.)
        let (doc_sender, doc_receiver) = unbounded();

        // Provide contexts to the Dioxus component tree
        let param_context = ParamContext::new(self.gui_context.clone(), self.needs_redraw.clone());
        let shared_state = self.shared_state.take();
        let dioxus_state_for_context = self.dioxus_state.clone();

        // Create DocumentProxy for document::Style support
        let doc_proxy = DocumentProxy::new(doc_sender);
        let doc_proxy_rc = Rc::new(doc_proxy);

        let initial_logical = self.dioxus_state.inner_logical_size();
        let window_size_signal = dioxus_doc.vdom.in_scope(ScopeId::ROOT, move || {
            // Provide DocumentProxy as Document for document::Style
            provide_context(doc_proxy_rc as Rc<dyn document::Document>);

            // Provide ParamContext for parameter bindings
            provide_context(param_context);

            // Inject DioxusState so ResizeHandle can access it
            provide_context(dioxus_state_for_context);

            // Provide OverlayRegistry for use_scene_overlay hook
            provide_context(overlay_registry);

            // Provide reactive window size signal for overlay positioning
            let sig = Signal::new(initial_logical);
            provide_context(sig);

            // Inject shared state if provided
            if let Some(state) = shared_state {
                provide_context(state);
            }

            sig
        });
        self.window_size_signal = Some(window_size_signal);

        // Initial build - this may queue document::Style messages
        dioxus_doc.initial_build();

        // Process any document messages that were queued during initial_build()
        // This is CRITICAL - CSS must be added to the stylist BEFORE resolve()
        let mut initial_head_count = 0usize;
        let mut initial_style_bytes = 0usize;
        while let Ok(msg) = doc_receiver.try_recv() {
            match msg {
                DocumentMessage::CreateHeadElement {
                    name,
                    attributes,
                    contents,
                } => {
                    initial_head_count += 1;
                    if name == "style" {
                        initial_style_bytes += contents.as_ref().map(|s| s.len()).unwrap_or(0);
                    }
                    let attrs: Vec<(String, String)> = attributes;
                    dioxus_doc.create_head_element(&name, &attrs, &contents);
                }
            }
        }
        if diagnostics_enabled() {
            eprintln!(
                "[NIH_DIOXUS_DIAG] initial_head_elements={initial_head_count} initial_style_bytes={initial_style_bytes}"
            );
        }

        // Store the receiver for processing messages during on_frame
        self.doc_message_receiver = Some(doc_receiver);

        // Now resolve layout (CSS is already added)
        dioxus_doc.inner_mut().resolve(0.0);

        self.wgpu_state = Some(wgpu_state);
        self.renderer = Some(renderer);
        self.dioxus_doc = Some(dioxus_doc);

        // Connect to hot reload server
        #[cfg(feature = "hot-reload")]
        self.hot_reload.connect();
    }

    /// Get the current animation time in seconds.
    fn animation_time(&self) -> f64 {
        self.animation_start.elapsed().as_secs_f64()
    }
}

/// The view's ACTUAL physical size and backing scale, read from AppKit.
///
/// Returns `(physical_width, physical_height, scale_factor)`.
///
/// baseview's cached values cannot be trusted on macOS for two reasons, and
/// both bite in REAPER. Nothing observes `NSViewFrameDidChange`, so when the
/// HOST resizes our view the cache is never updated and the editor keeps
/// rendering at the old size until something else happens to refresh it — the
/// "editor only catches up once you finish dragging" behaviour. And the scale
/// is captured once at window creation as
/// `parent_view.window()…unwrap_or(1.0)`, so a view parented before it belongs
/// to a window is stuck at 1.0 forever — on a Retina display that renders the
/// whole editor at half size.
///
/// Asking AppKit directly each frame sidesteps both.
#[cfg(target_os = "macos")]
struct MacGeometry {
    physical_w: u32,
    physical_h: u32,
    scale: f32,
    /// Whether the view actually belongs to a window yet. Until it does, the
    /// backing scale below is a fallback, not a measurement.
    in_window: bool,
}

#[cfg(target_os = "macos")]
fn macos_live_geometry(handle: &RawWindowHandle) -> Option<MacGeometry> {
    use objc2_app_kit::NSView;

    let RawWindowHandle::AppKit(appkit) = handle else {
        return None;
    };
    // SAFETY: baseview owns this NSView and keeps it alive for the editor's
    // lifetime; we only read geometry, on the main thread.
    let view: &NSView = unsafe { appkit.ns_view.cast().as_ref() };
    let window = view.window();
    let in_window = window.is_some();
    let scale = window.map(|w| w.backingScaleFactor()).unwrap_or(1.0);

    // `convertRectToBacking` is the authoritative point→backing-pixel
    // conversion: it is what AppKit itself uses to size the backing store, and
    // unlike `frame * backingScaleFactor` it cannot double-count a scale that
    // some ancestor has already applied.
    let bounds = view.bounds();
    let backing = view.convertRectToBacking(bounds);

    if diagnostics_enabled() {
        nice_plug_core::nice_log!(
            "[GEOM] frame={}x{} bounds={}x{} backing={}x{} scale={} in_window={}",
            view.frame().size.width,
            view.frame().size.height,
            bounds.size.width,
            bounds.size.height,
            backing.size.width,
            backing.size.height,
            scale,
            in_window
        );
    }

    Some(MacGeometry {
        physical_w: (backing.size.width.round() as u32).max(1),
        physical_h: (backing.size.height.round() as u32).max(1),
        scale: scale as f32,
        in_window,
    })
}

impl HandlerState {
    /// Track the view's real geometry on macOS, where no resize event comes.
    ///
    /// Cheap: two AppKit property reads per frame, and it only does work when
    /// something actually changed.
    #[cfg(target_os = "macos")]
    fn poll_macos_geometry(&mut self) {
        let Some(handle) = self.window_handle else {
            return;
        };
        let Some(geom) = macos_live_geometry(&handle) else {
            return;
        };
        let changed = geom.physical_w != self.width
            || geom.physical_h != self.height
            || (geom.scale - self.scale_factor).abs() > f32::EPSILON;
        if !changed {
            return;
        }
        let logical_w = (geom.physical_w as f32 / geom.scale).round() as u32;
        let logical_h = (geom.physical_h as f32 / geom.scale).round() as u32;
        self.apply_physical_size(
            geom.physical_w,
            geom.physical_h,
            geom.scale,
            logical_w,
            logical_h,
        );
    }

    /// The geometry to initialize with, and whether it is trustworthy yet.
    ///
    /// On macOS the backing scale is only knowable once the view belongs to a
    /// window — before that `backingScaleFactor()` has nothing to report and
    /// baseview's cached value is its `unwrap_or(1.0)` fallback. Initializing
    /// then builds the surface and the Blitz viewport at scale 1, so on a
    /// Retina display the editor comes up at half size and stays there until
    /// something else forces a resize. That is the "wrong size until I nudge
    /// the window" behaviour.
    ///
    /// So: wait for the view to be in a window. Bounded, because a host that
    /// never puts it in one must still get a painted editor rather than the
    /// blank grey we just finished fixing.
    #[cfg(target_os = "macos")]
    fn init_geometry(&mut self, window: &WindowContext) -> Option<(u32, u32, f32, u32, u32)> {
        const MAX_WAIT_FRAMES: u32 = 120; // ~2s at the 15ms frame timer.

        let live = self.window_handle.and_then(|h| macos_live_geometry(&h));
        match live {
            Some(geom) if geom.in_window => {
                let logical_w = (geom.physical_w as f32 / geom.scale).round() as u32;
                let logical_h = (geom.physical_h as f32 / geom.scale).round() as u32;
                Some((
                    geom.physical_w,
                    geom.physical_h,
                    geom.scale,
                    logical_w,
                    logical_h,
                ))
            }
            _ if self.init_wait_frames < MAX_WAIT_FRAMES => {
                self.init_wait_frames += 1;
                None
            }
            _ => {
                nice_plug_core::nice_warn!(
                    "[INIT] view never joined a window after {} frames — initializing at the \
                     scale baseview reported, which may be wrong on a HiDPI display",
                    MAX_WAIT_FRAMES
                );
                let size = window.size();
                Some((
                    size.physical.width,
                    size.physical.height,
                    size.scale_factor as f32,
                    size.logical.width as u32,
                    size.logical.height as u32,
                ))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn init_geometry(&mut self, window: &WindowContext) -> Option<(u32, u32, f32, u32, u32)> {
        let size = window.size();
        Some((
            size.physical.width,
            size.physical.height,
            size.scale_factor as f32,
            size.logical.width as u32,
            size.logical.height as u32,
        ))
    }

    #[cfg(not(target_os = "macos"))]
    fn poll_macos_geometry(&mut self) {}

    fn on_frame(&mut self, window: &WindowContext) {
        // Initialization needs the window's real physical size and scale. This
        // used to wait for the first `resized()` callback to supply them, which
        // works on X11 — the ConfigureNotify following window creation always
        // delivers one — and never happens on macOS: baseview only calls
        // `resized()` from `view_did_change_backing_properties`, and only when
        // the size or scale actually CHANGES from what the view was created
        // with. A freshly created editor has changed neither, so no event ever
        // came, `received_resize` stayed false, this returned early on every
        // frame, and wgpu was never created. That is the blank grey editor on
        // macOS — no [INIT], no surface, nothing painted, no crash to point at.
        //
        // The wait was never necessary: the geometry can be asked for. What
        // DOES have to be waited for on macOS is the view joining a window,
        // without which the backing scale is unknowable — see `init_geometry`.
        if self.wgpu_state.is_none() {
            if !self.received_resize {
                let Some((physical_w, physical_h, scale, logical_w, logical_h)) =
                    self.init_geometry(window)
                else {
                    return; // not ready to know our scale yet
                };
                self.width = physical_w;
                self.height = physical_h;
                self.scale_factor = scale;
                self.dioxus_state.set_size(logical_w, logical_h);
                self.received_resize = true;
            }
            self.initialize();
        }

        // macOS never tells us the host resized us, and may have handed us a
        // scale of 1.0 for a view that was not in a window yet. Ask AppKit.
        self.poll_macos_geometry();

        // Check for pending resize request from the UI (UI provides LOGICAL size).
        // We only issue the resize request here — the actual width/height, viewport,
        // and wgpu state are updated when the Resized event arrives from the window
        // system (ConfigureNotify on X11). Updating eagerly would cause the renderer
        // to draw at a size that doesn't match the actual window.
        if let Some((new_logical_width, new_logical_height)) =
            self.dioxus_state.take_pending_resize()
        {
            nice_plug_core::nice_log!(
                "[RESIZE] Pending resize: {}x{} logical (current physical: {}x{})",
                new_logical_width,
                new_logical_height,
                self.width,
                self.height
            );

            // Sanity check - don't resize to crazy values (in logical pixels)
            if new_logical_width > 4096
                || new_logical_height > 4096
                || new_logical_width < 100
                || new_logical_height < 100
            {
                nice_plug_core::nice_warn!(
                    "[RESIZE] Ignoring invalid size: {}x{}",
                    new_logical_width,
                    new_logical_height
                );
            } else {
                // Ask for the resize. Under baseview 0.3 this is the whole
                // plugin-driven path: baseview forwards the request to the host
                // through `HostCallbacks::request_resize` (wired up in
                // `editor.rs`), and the granted size comes back as a `resized()`
                // callback. There is no separate `GuiContext::request_resize`
                // to call any more — doing both is what used to cause resize
                // feedback loops.
                if let Err(e) = window.resize(LogicalSize::new(
                    new_logical_width as f64,
                    new_logical_height as f64,
                )) {
                    nice_plug_core::nice_warn!(
                        "[RESIZE] Host refused {}x{}: {e}",
                        new_logical_width,
                        new_logical_height
                    );
                } else {
                    // Store logical size for persistence / host query.
                    self.dioxus_state
                        .set_size(new_logical_width, new_logical_height);
                }
            }
        }

        // Check for host-driven resize (set_size from host — no callback to host)
        if let Some((new_logical_width, new_logical_height)) =
            self.dioxus_state.take_pending_host_resize()
        {
            let new_physical_width = (new_logical_width as f32 * self.scale_factor) as u32;
            let new_physical_height = (new_logical_height as f32 * self.scale_factor) as u32;

            // Resize the baseview window (needed for the child NSView to match).
            // This one is host-driven, so a failure is only worth logging — the
            // host already owns the size it told us about.
            if let Err(e) = window.resize(LogicalSize::new(
                new_logical_width as f64,
                new_logical_height as f64,
            )) {
                nice_plug_core::nice_warn!("[RESIZE] Host-driven resize failed: {e}");
            }

            self.width = new_physical_width;
            self.height = new_physical_height;

            self.dioxus_state
                .set_size(new_logical_width, new_logical_height);

            // NOTE: Do NOT call gui_context.request_resize() here — the host
            // is already driving this resize, calling back would create a loop.

            if let Some(doc) = &mut self.dioxus_doc {
                doc.inner_mut().set_viewport(Viewport::new(
                    new_physical_width,
                    new_physical_height,
                    self.scale_factor,
                    ColorScheme::Light,
                ));
            }

            if let Some(wgpu_state) = &mut self.wgpu_state {
                wgpu_state.resize(new_physical_width, new_physical_height);
            }

            self.needs_redraw.store(true, Ordering::Relaxed);
        }

        // The host has just parented or mapped the window: reconfigure the
        // surface and redraw, which is what a manual resize was doing for us.
        if self.dioxus_state.take_revalidate() {
            if let Some(wgpu_state) = &mut self.wgpu_state {
                wgpu_state.resize(self.width, self.height);
            }
            self.needs_redraw.store(true, Ordering::Relaxed);
        }

        // Measure time since last frame (gap between on_frame calls)
        let t_gap = self.last_frame_start.elapsed().as_millis();
        self.last_frame_start = Instant::now();

        // Get animation time upfront before any mutable borrows
        let animation_time = self.animation_start.elapsed().as_secs_f64();
        let needs_redraw = self.needs_redraw.clone();
        let scale_factor = self.scale_factor;

        // self.width and self.height are already in physical pixels
        // Cap at 4096 to avoid Vello's texture size limits
        // See: https://github.com/linebender/vello/issues/680
        const MAX_RENDER_SIZE: u32 = 4096;
        let physical_width = self.width.min(MAX_RENDER_SIZE);
        let physical_height = self.height.min(MAX_RENDER_SIZE);

        let Some(doc) = &mut self.dioxus_doc else {
            return;
        };
        let Some(wgpu_state) = &mut self.wgpu_state else {
            return;
        };
        // If configure failed during init, retry each frame (non-blocking).
        // The X11 event loop runs normally between frames so Vulkan gets the
        // events it needs to successfully create the swapchain.
        if !wgpu_state.try_configure() {
            return;
        }
        let wgpu_state = &*wgpu_state;
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        // Handle hot reload messages
        #[cfg(feature = "hot-reload")]
        self.hot_reload.process_messages(doc);

        // Process any pending document messages (e.g., dynamically added styles)
        if let Some(receiver) = &self.doc_message_receiver {
            let mut head_count = 0usize;
            let mut style_bytes = 0usize;
            while let Ok(msg) = receiver.try_recv() {
                match msg {
                    DocumentMessage::CreateHeadElement {
                        name,
                        attributes,
                        contents,
                    } => {
                        head_count += 1;
                        if name == "style" {
                            style_bytes += contents.as_ref().map(|s| s.len()).unwrap_or(0);
                        }
                        let attrs: Vec<(String, String)> = attributes;
                        doc.create_head_element(&name, &attrs, &contents);
                    }
                }
            }
            if diagnostics_enabled() && head_count > 0 {
                eprintln!(
                    "[NIH_DIOXUS_DIAG] frame_head_elements={head_count} frame_style_bytes={style_bytes}"
                );
            }
        }

        // Create a waker that triggers redraw
        let waker = futures_util::task::waker(Arc::new(RedrawWaker(needs_redraw.clone())));

        // Throttle DOM update (mark_all_dirty → poll → resolve) to ~30fps.
        // This prevents Taffy from doing a full flexbox relayout every frame,
        // which was costing 73-465ms and capping FPS at ~3. Rendering from
        // the existing layout runs at full frame rate between DOM updates.
        const DOM_UPDATE_INTERVAL_MS: u128 = 33; // ~30fps
        let do_dom_update = self.last_dom_update.elapsed().as_millis() >= DOM_UPDATE_INTERVAL_MS
            || needs_redraw.load(Ordering::Relaxed);

        let t_dirty;
        let t_poll;
        let t_resolve;

        if do_dom_update {
            let t0 = Instant::now();
            doc.vdom.mark_all_dirty();
            t_dirty = t0.elapsed().as_millis();

            let t1 = Instant::now();
            let cx = std::task::Context::from_waker(&waker);
            doc.poll(Some(cx));
            t_poll = t1.elapsed().as_millis();

            let t2 = Instant::now();
            doc.inner_mut().resolve(animation_time);
            t_resolve = t2.elapsed().as_millis();

            self.last_dom_update = Instant::now();
        } else {
            // Just poll for async events (user interactions) without full relayout
            let cx = std::task::Context::from_waker(&waker);
            doc.poll(Some(cx));
            t_dirty = 0;
            t_poll = 0;
            t_resolve = 0;
        }

        // Render at physical size
        let t3 = Instant::now();
        renderer.render(
            wgpu_state,
            doc,
            scale_factor,
            physical_width,
            physical_height,
        );
        let t_render = t3.elapsed().as_millis();

        // FPS + frame timing log (every second)
        self.fps_frame_count += 1;
        let elapsed = self.fps_last_report.elapsed();
        if elapsed.as_secs_f32() >= 1.0 {
            let fps = self.fps_frame_count as f32 / elapsed.as_secs_f32();
            let dom_nodes = doc.inner().tree().len();
            let dom_update_str = if do_dom_update { "Y" } else { "N" };
            eprintln!("[FPS-v2] {fps:.1} fps | gap={t_gap}ms dirty={t_dirty}ms poll={t_poll}ms resolve={t_resolve}ms render={t_render}ms | dom={dom_nodes} updated={dom_update_str}");
            self.fps_frame_count = 0;
            self.fps_last_report = Instant::now();
        }

        // Apply any cursor change requested by blitz (e.g. hovering over a button)
        if let Some(icon) = self.pending_cursor.lock().unwrap().take() {
            let _ = window.set_mouse_cursor(cursor_icon_to_baseview(icon));
        }

        // Reset redraw flag
        self.needs_redraw.store(false, Ordering::Relaxed);
    }

    /// The window was resized — by the host, or by our own `resize()` request
    /// being granted.
    ///
    /// In baseview 0.3 this is a handler method rather than a `WindowEvent`, so
    /// it is the single place the physical size, the Blitz viewport, and the
    /// wgpu surface are kept in step.
    fn resized(&mut self, new_size: baseview::WindowSize) {
        self.apply_physical_size(
            new_size.physical.width,
            new_size.physical.height,
            new_size.scale_factor as f32,
            new_size.logical.width as u32,
            new_size.logical.height as u32,
        );
    }

    /// Adopt a new physical size and scale: wgpu surface, Blitz viewport and
    /// the persisted logical size all move together.
    ///
    /// Split out of `resized` so the macOS geometry poll can drive the same
    /// path — on that platform baseview does not report a parent-driven
    /// resize at all, so this is the only way the editor ever hears about one.
    fn apply_physical_size(
        &mut self,
        physical_w: u32,
        physical_h: u32,
        scale_factor: f32,
        logical_w: u32,
        logical_h: u32,
    ) {
        // PHYSICAL size drives wgpu and the Blitz viewport.
        self.width = physical_w;
        self.height = physical_h;
        self.scale_factor = scale_factor;
        self.received_resize = true;

        nice_plug_core::nice_log!(
            "[RESIZE] physical: {}x{}, logical: {}x{}, scale: {}",
            self.width,
            self.height,
            logical_w,
            logical_h,
            self.scale_factor
        );

        // LOGICAL size is what gets persisted.
        self.dioxus_state.set_size(logical_w, logical_h);

        // Let overlay components re-query their rects.
        if let Some(mut sig) = self.window_size_signal {
            sig.set((logical_w, logical_h));
        }

        if let Some(doc) = &mut self.dioxus_doc {
            doc.inner_mut().set_viewport(Viewport::new(
                self.width,
                self.height,
                self.scale_factor,
                ColorScheme::Light,
            ));
        }

        if let Some(wgpu_state) = &mut self.wgpu_state {
            wgpu_state.resize(self.width, self.height);
        }

        self.needs_redraw.store(true, Ordering::Relaxed);
    }

    fn on_event(&mut self, _window: &WindowContext, event: Event) -> EventStatus {

        // Translate and dispatch event to Dioxus
        if let Some(doc) = &mut self.dioxus_doc {
            if let Some(ui_event) = translate_event(
                &event,
                &mut self.mouse_pos,
                &mut self.mouse_buttons,
                &mut self.modifiers,
                (self.width, self.height),
            ) {
                // Debug log for mouse events with hit testing info
                match &ui_event {
                    blitz_traits::events::UiEvent::PointerDown(e) => {
                        let (x, y) = (e.coords.client_x, e.coords.client_y);
                        nice_plug_core::nice_log!("[CLICK] PointerDown at ({}, {})", x, y);
                        // Try to get hit test info
                        let inner = doc.inner();
                        if let Some(hit) = inner.hit(x, y) {
                            if let Some(node) = inner.get_node(hit.node_id) {
                                let tag = node
                                    .element_data()
                                    .map(|ed| ed.name.local.as_ref())
                                    .unwrap_or("?");
                                // Log all attributes to debug
                                let attrs: Vec<String> = node
                                    .element_data()
                                    .map(|ed| {
                                        ed.attrs()
                                            .iter()
                                            .map(|a| {
                                                format!(
                                                    "{}={}",
                                                    a.name.local,
                                                    a.value.chars().take(20).collect::<String>()
                                                )
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                nice_plug_core::nice_log!(
                                    "[HIT] Node {} tag={} attrs=[{}]",
                                    hit.node_id,
                                    tag,
                                    attrs.join(", ")
                                );
                            }
                        }
                    }
                    blitz_traits::events::UiEvent::PointerUp(e) => {
                        nice_plug_core::nice_log!(
                            "[CLICK] PointerUp at ({}, {})",
                            e.coords.client_x,
                            e.coords.client_y
                        );
                    }
                    blitz_traits::events::UiEvent::PointerMove(e) => {
                        let (x, y) = (e.coords.client_x, e.coords.client_y);
                        // Log hover only occasionally to avoid spam (every ~50 pixels of movement)
                        static LAST_LOG: std::sync::atomic::AtomicU32 =
                            std::sync::atomic::AtomicU32::new(0);
                        let pos_hash = ((x as u32) / 50) * 1000 + ((y as u32) / 50);
                        let last = LAST_LOG.load(std::sync::atomic::Ordering::Relaxed);
                        if pos_hash != last {
                            LAST_LOG.store(pos_hash, std::sync::atomic::Ordering::Relaxed);
                            let inner = doc.inner();
                            if let Some(hit) = inner.hit(x, y) {
                                if let Some(node) = inner.get_node(hit.node_id) {
                                    let tag = node
                                        .element_data()
                                        .map(|ed| ed.name.local.as_ref())
                                        .unwrap_or("?");
                                    let class = node
                                        .element_data()
                                        .and_then(|ed| {
                                            ed.attrs()
                                                .iter()
                                                .find(|a| a.name.local.as_ref() == "class")
                                        })
                                        .map(|a| a.value.chars().take(30).collect::<String>())
                                        .unwrap_or_default();
                                    nice_plug_core::nice_log!(
                                        "[HOVER] ({:.0}, {:.0}) -> Node {} tag={} class={}",
                                        x,
                                        y,
                                        hit.node_id,
                                        tag,
                                        class
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
                doc.handle_ui_event(ui_event);
                self.needs_redraw.store(true, Ordering::Relaxed);
                return EventStatus::Captured;
            }
        }

        EventStatus::Ignored
    }
}

/// Waker that sets a flag to trigger a redraw.
struct RedrawWaker(Arc<AtomicBool>);

/// baseview 0.3 drives the handler through `&self`, so every entry point takes
/// the state out of the `RefCell` and hands the captured window to the real
/// implementation on [`HandlerState`].
///
/// Re-entrancy is the one hazard: a borrow that is still live when baseview
/// calls back in would panic. `try_borrow_mut` degrades to skipping the call
/// instead — a dropped frame or event is recoverable, a panic across the FFI
/// boundary is not.
impl WindowHandler for DioxusWindowHandler {
    fn on_frame(&self) -> Result<(), HandlerError> {
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.on_frame(&self.window);
        }
        Ok(())
    }

    fn resized(&self, new_size: baseview::WindowSize) -> Result<(), HandlerError> {
        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.resized(new_size);
        }
        Ok(())
    }

    fn on_event(&self, event: Event) -> EventStatus {
        match self.state.try_borrow_mut() {
            Ok(mut state) => state.on_event(&self.window, event),
            Err(_) => EventStatus::Ignored,
        }
    }
}

impl ArcWake for RedrawWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.store(true, Ordering::Relaxed);
    }
}

/// Get raw window handles from a baseview [`WindowContext`].
///
/// baseview 0.3 implements the raw-window-handle 0.6 traits on `WindowContext`
/// directly, which is what wgpu and softbuffer want.
fn get_raw_handles_from_baseview(
    window: &WindowContext,
) -> (Option<RawWindowHandle>, Option<RawDisplayHandle>) {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

    // Get the 0.6 handles directly from baseview
    let window_handle = window.window_handle().ok().map(|h| h.as_raw());
    let display_handle = window.display_handle().ok().map(|h| h.as_raw());

    // Debug: log the raw handles
    nice_plug_core::nice_log!(
        "[RAW HANDLES] window: {:?}, display: {:?}",
        window_handle,
        display_handle
    );

    (window_handle, display_handle)
}
