//! Baseview window handler using offscreen wgpu + softbuffer blit.
//!
//! This module provides an alternative window handler that renders with wgpu
//! to an offscreen texture, then blits the result to the window using softbuffer.
//! This works on Linux/XWayland where direct wgpu surface creation fails.

use crate::context::ParamContext;
use crate::events::translate_event;
use crate::renderer::Renderer;
use crate::state::DioxusState;
use crate::wgpu_offscreen::WgpuOffscreenState;
use crate::SharedState;

#[cfg(feature = "hot-reload")]
use crate::hot_reload::HotReloadState;

use baseview::{Event, EventStatus, Window, WindowHandler};
use blitz_dom::{Document as _, DocumentConfig};
use blitz_traits::events::MouseEventButtons;
use blitz_traits::shell::{ColorScheme, Viewport};
use crossbeam::channel::{unbounded, Receiver, Sender};
use dioxus_native::prelude::*;
use dioxus_native::DioxusDocument;
use futures_util::task::ArcWake;
use nice_plug_core::context::gui::GuiContext;

use crate::events::Modifiers;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Messages sent from Dioxus components to the window handler.
enum DocumentMessage {
    CreateHeadElement {
        name: String,
        attributes: Vec<(String, String)>,
        contents: Option<String>,
    },
}

/// Proxy for document operations from Dioxus components.
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

impl document::Document for DocumentProxy {
    fn eval(&self, js: String) -> document::Eval {
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

/// Baseview window handler using offscreen wgpu + softbuffer blit.
pub struct DioxusSoftbufferWindowHandler {
    // Dioxus state
    dioxus_doc: Option<DioxusDocument>,
    app: fn() -> Element,
    animation_start: Instant,

    // Offscreen wgpu rendering
    wgpu_state: Option<WgpuOffscreenState>,
    renderer: Option<Renderer>,

    // nice-plug integration
    gui_context: Arc<dyn GuiContext>,
    dioxus_state: Arc<DioxusState>,
    needs_redraw: Arc<AtomicBool>,

    // Shared UI state
    shared_state: Option<SharedState>,

    // Document message channel
    doc_message_receiver: Option<Receiver<DocumentMessage>>,

    // Input state
    mouse_pos: (f32, f32),
    mouse_buttons: MouseEventButtons,
    modifiers: Modifiers,

    // Hot reload
    #[cfg(feature = "hot-reload")]
    hot_reload: HotReloadState,

    // Window dimensions
    width: u32,
    height: u32,
    scale_factor: f32,
    received_resize: bool,
}

impl DioxusSoftbufferWindowHandler {
    /// Create a new window handler.
    pub fn new(
        _window: &mut Window,
        app: fn() -> Element,
        gui_context: Arc<dyn GuiContext>,
        dioxus_state: Arc<DioxusState>,
        needs_redraw: Arc<AtomicBool>,
    ) -> Self {
        Self::new_with_state(_window, app, gui_context, dioxus_state, needs_redraw, None)
    }

    /// Create a new window handler with shared state.
    pub fn new_with_state(
        _window: &mut Window,
        app: fn() -> Element,
        gui_context: Arc<dyn GuiContext>,
        dioxus_state: Arc<DioxusState>,
        needs_redraw: Arc<AtomicBool>,
        shared_state: Option<SharedState>,
    ) -> Self {
        let (logical_width, logical_height) = dioxus_state.inner_logical_size();
        let scale_factor = 1.0f32;
        let physical_width = (logical_width as f32 * scale_factor) as u32;
        let physical_height = (logical_height as f32 * scale_factor) as u32;

        nice_plug_core::nice_log!(
            "[Softbuffer] Creating window handler {}x{}",
            physical_width,
            physical_height
        );

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
            #[cfg(feature = "hot-reload")]
            hot_reload: HotReloadState::new(),
            width: physical_width,
            height: physical_height,
            scale_factor,
            received_resize: false,
        }
    }

    /// Initialize the Dioxus document and rendering state.
    fn initialize(&mut self) {
        let physical_width = self.width.max(1);
        let physical_height = self.height.max(1);

        nice_plug_core::nice_log!(
            "[Softbuffer INIT] physical: {}x{}, scale: {}",
            physical_width,
            physical_height,
            self.scale_factor
        );

        // Create offscreen wgpu state (no surface needed!)
        let wgpu_state = WgpuOffscreenState::new(physical_width, physical_height);

        // Create renderer
        let renderer = Renderer::new(&wgpu_state.device);
        let overlay_registry = renderer.overlay_registry();

        // Create the Dioxus virtual DOM
        let vdom = VirtualDom::new(self.app);

        // Create viewport
        let viewport = Viewport::new(
            self.width,
            self.height,
            self.scale_factor,
            ColorScheme::Light,
        );

        // Create the Dioxus document
        let mut dioxus_doc = DioxusDocument::new(
            vdom,
            DocumentConfig {
                viewport: Some(viewport),
                ..Default::default()
            },
        );

        // Create channel for document messages
        let (doc_sender, doc_receiver) = unbounded();

        // Provide contexts to the Dioxus component tree
        let param_context = ParamContext::new(self.gui_context.clone(), self.needs_redraw.clone());
        let shared_state = self.shared_state.take();
        let dioxus_state_for_context = self.dioxus_state.clone();

        let doc_proxy = DocumentProxy::new(doc_sender);
        let doc_proxy_rc = Rc::new(doc_proxy);

        dioxus_doc.vdom.in_scope(ScopeId::ROOT, move || {
            provide_context(doc_proxy_rc as Rc<dyn document::Document>);
            provide_context(param_context);
            provide_context(dioxus_state_for_context);
            provide_context(overlay_registry);

            if let Some(state) = shared_state {
                provide_context(state);
            }
        });

        // Initial build
        dioxus_doc.initial_build();

        // Process document messages
        while let Ok(msg) = doc_receiver.try_recv() {
            match msg {
                DocumentMessage::CreateHeadElement {
                    name,
                    attributes,
                    contents,
                } => {
                    let attrs: Vec<(String, String)> = attributes;
                    dioxus_doc.create_head_element(&name, &attrs, &contents);
                }
            }
        }

        self.doc_message_receiver = Some(doc_receiver);

        // Resolve layout
        dioxus_doc.inner_mut().resolve(0.0);

        self.wgpu_state = Some(wgpu_state);
        self.renderer = Some(renderer);
        self.dioxus_doc = Some(dioxus_doc);

        #[cfg(feature = "hot-reload")]
        self.hot_reload.connect();

        nice_plug_core::nice_log!("[Softbuffer] Initialization complete");
    }
}

impl WindowHandler for DioxusSoftbufferWindowHandler {
    fn on_frame(&mut self, window: &mut Window) {
        // Initialize after receiving the first resize event
        if self.wgpu_state.is_none() {
            if self.received_resize {
                self.initialize();
            } else {
                return;
            }
        }

        // Check for pending resize request from the UI (UI provides LOGICAL size).
        // We only issue the resize request here — the actual width/height, viewport,
        // and wgpu state are updated when the Resized event arrives from the window
        // system (ConfigureNotify on X11). Updating eagerly would cause the renderer
        // to draw at a size that doesn't match the actual window, resulting in black
        // regions or clipped content.
        if let Some((new_logical_width, new_logical_height)) =
            self.dioxus_state.take_pending_resize()
        {
            nice_plug_core::nice_log!(
                "[Softbuffer RESIZE] Pending resize: {}x{} logical (current physical: {}x{})",
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
                    "[Softbuffer RESIZE] Ignoring invalid size: {}x{}",
                    new_logical_width,
                    new_logical_height
                );
            } else {
                // Request the window resize (async — X11 will send ConfigureNotify)
                window.resize(baseview::Size::new(
                    new_logical_width as f64,
                    new_logical_height as f64,
                ));

                // Store logical size for persistence / host query
                self.dioxus_state
                    .set_size(new_logical_width, new_logical_height);

                // Notify the host that the window size changed
                self.gui_context.request_resize();
            }
        }

        // Check for host-driven resize (e.g. user dragging the plugin window border
        // in the DAW). The host already knows the new size, so do NOT call
        // gui_context.request_resize() — that would create an infinite loop.
        if let Some((new_logical_width, new_logical_height)) =
            self.dioxus_state.take_pending_host_resize()
        {
            if new_logical_width >= 100
                && new_logical_height >= 100
                && new_logical_width <= 4096
                && new_logical_height <= 4096
            {
                let new_physical_width = (new_logical_width as f32 * self.scale_factor) as u32;
                let new_physical_height = (new_logical_height as f32 * self.scale_factor) as u32;

                window.resize(baseview::Size::new(
                    new_logical_width as f64,
                    new_logical_height as f64,
                ));

                self.width = new_physical_width;
                self.height = new_physical_height;

                self.dioxus_state
                    .set_size(new_logical_width, new_logical_height);

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
        }

        let animation_time = self.animation_start.elapsed().as_secs_f64();
        let needs_redraw = self.needs_redraw.clone();
        let scale_factor = self.scale_factor;

        const MAX_RENDER_SIZE: u32 = 4096;
        let physical_width = self.width.min(MAX_RENDER_SIZE);
        let physical_height = self.height.min(MAX_RENDER_SIZE);

        let Some(doc) = &mut self.dioxus_doc else {
            return;
        };
        let Some(wgpu_state) = &mut self.wgpu_state else {
            return;
        };
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        #[cfg(feature = "hot-reload")]
        self.hot_reload.process_messages(doc);

        // Process document messages
        if let Some(receiver) = &self.doc_message_receiver {
            while let Ok(msg) = receiver.try_recv() {
                match msg {
                    DocumentMessage::CreateHeadElement {
                        name,
                        attributes,
                        contents,
                    } => {
                        let attrs: Vec<(String, String)> = attributes;
                        doc.create_head_element(&name, &attrs, &contents);
                    }
                }
            }
        }

        // Force ALL scopes to re-render every frame so metering/viz data
        // (read from atomics) stays up to date. We must mark all scopes
        // because ScopeId::ROOT is a RootScopeWrapper whose children are
        // memoized (SuspenseBoundary/ErrorBoundary), so marking only ROOT
        // doesn't propagate to the actual App component.
        doc.vdom.mark_all_dirty();

        // Poll the virtual DOM
        let waker = futures_util::task::waker(Arc::new(RedrawWaker(needs_redraw.clone())));
        let cx = std::task::Context::from_waker(&waker);
        let did_render = doc.poll(Some(cx));

        static POLL_LOG_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = POLL_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
        if count % 300 == 0 {
            nice_plug_core::nice_log!("[Softbuffer] poll={} frame={}", did_render, count);
        }

        // Resolve layout
        doc.inner_mut().resolve(animation_time);

        // Render to offscreen texture
        renderer.render_offscreen(
            wgpu_state,
            doc,
            scale_factor,
            physical_width,
            physical_height,
        );

        // Read pixels back from GPU
        let pixels = wgpu_state.read_pixels();

        // Convert to softbuffer format and blit to window
        let pixel_data =
            WgpuOffscreenState::bgra_to_softbuffer(&pixels, physical_width, physical_height);

        // Create softbuffer context and surface for this frame
        // Note: We use window reference for both Context (display) and Surface (window)
        let window_ref = &*window;
        if let Ok(context) = softbuffer::Context::new(window_ref) {
            if let Ok(mut surface) = softbuffer::Surface::new(&context, window_ref) {
                if let (Some(w), Some(h)) = (
                    NonZeroU32::new(physical_width),
                    NonZeroU32::new(physical_height),
                ) {
                    if surface.resize(w, h).is_ok() {
                        if let Ok(mut buffer) = surface.buffer_mut() {
                            // Copy pixel data to softbuffer
                            buffer.copy_from_slice(&pixel_data);
                            let _ = buffer.present();
                        }
                    }
                }
            }
        }

        self.needs_redraw.store(false, Ordering::Relaxed);
    }

    fn on_event(&mut self, _window: &mut Window, event: Event) -> EventStatus {
        match &event {
            Event::Window(baseview::WindowEvent::Resized(info)) => {
                let physical_size = info.physical_size();
                self.width = physical_size.width as u32;
                self.height = physical_size.height as u32;
                self.scale_factor = info.scale() as f32;
                self.received_resize = true;

                nice_plug_core::nice_log!(
                    "[Softbuffer RESIZE] physical: {}x{}, scale: {}",
                    self.width,
                    self.height,
                    self.scale_factor
                );

                let logical_size = info.logical_size();
                self.dioxus_state
                    .set_size(logical_size.width as u32, logical_size.height as u32);

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
                return EventStatus::Captured;
            }
            _ => {}
        }

        // Translate and dispatch event to Dioxus
        if let Some(doc) = &mut self.dioxus_doc {
            if let Some(ui_event) = translate_event(
                &event,
                &mut self.mouse_pos,
                &mut self.mouse_buttons,
                &mut self.modifiers,
                (self.width, self.height),
            ) {
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

impl ArcWake for RedrawWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.store(true, Ordering::Relaxed);
    }
}
