//! Standalone example for testing nih_plug_dioxus without a DAW.
//!
//! This example demonstrates using lumen-blocks UI components with the
//! nih_plug_dioxus framework.
//!
//! Run with: cargo run -p nih_plug_dioxus --example standalone

use baseview::{Size, Window, WindowHandler, WindowOpenOptions, WindowScalePolicy};
use blitz_dom::{Document as BlitzDocument, DocumentConfig};
use blitz_traits::events::MouseEventButtons;
use blitz_traits::shell::{ColorScheme, Viewport};
use crossbeam::channel::{unbounded, Receiver, Sender};
use dioxus::prelude::*;
use dioxus_core::ScopeId;
use dioxus_native_dom::DioxusDocument;
use futures_util::task::ArcWake;
// Import raw-window-handle 0.5 traits (what baseview uses)
use raw_window_handle_05::{HasRawDisplayHandle, HasRawWindowHandle};
// Import raw-window-handle 0.6 types (what wgpu uses)
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

// Import from our crate
use nih_plug_dioxus::dioxus;
use nih_plug_dioxus::THEME_CSS;

// Import lumen-blocks components
use lumen_blocks::components::button::{Button, ButtonSize, ButtonVariant};
use lumen_blocks::components::progress::{Progress, ProgressSize, ProgressVariant};
use lumen_blocks::components::switch::Switch;

/// Messages sent from the Dioxus app to the window handler
enum WindowMessage {
    CreateHeadElement {
        name: String,
        attributes: Vec<(String, String)>,
        contents: Option<String>,
    },
    /// Request to resize the window (logical size)
    ResizeWindow { width: f64, height: f64 },
    /// Start drag-to-resize mode - window handler will track mouse globally
    StartResizeDrag { min_width: f64, min_height: f64 },
    /// Stop drag-to-resize mode
    StopResizeDrag,
}

/// Proxy for communicating with the window handler from Dioxus
/// Handles document operations (Style, Stylesheet) and window operations (resize)
#[derive(Clone)]
struct WindowProxy {
    sender: Sender<WindowMessage>,
}

impl WindowProxy {
    fn new(sender: Sender<WindowMessage>) -> Self {
        Self { sender }
    }

    fn create_head_element(
        &self,
        name: &str,
        attributes: &[(&str, String)],
        contents: Option<String>,
    ) {
        let _ = self.sender.send(WindowMessage::CreateHeadElement {
            name: name.to_string(),
            attributes: attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            contents,
        });
    }

    /// Request the window to resize to the given logical dimensions
    pub fn resize_window(&self, width: f64, height: f64) {
        let _ = self
            .sender
            .send(WindowMessage::ResizeWindow { width, height });
    }

    /// Start drag-to-resize mode - the window handler will track mouse globally
    pub fn start_resize_drag(&self, min_width: f64, min_height: f64) {
        let _ = self.sender.send(WindowMessage::StartResizeDrag {
            min_width,
            min_height,
        });
    }

    /// Stop drag-to-resize mode
    pub fn stop_resize_drag(&self) {
        let _ = self.sender.send(WindowMessage::StopResizeDrag);
    }
}

/// Hook to get access to window operations like resize
fn use_window_proxy() -> WindowProxy {
    use_context::<WindowProxy>()
}

impl dioxus::document::Document for WindowProxy {
    fn eval(&self, js: String) -> dioxus::document::Eval {
        // No-op for native - we don't support JS eval
        dioxus::document::NoOpDocument.eval(js)
    }

    fn set_title(&self, title: String) {
        self.create_head_element("title", &[], Some(title));
    }

    fn create_meta(&self, props: dioxus::document::MetaProps) {
        self.create_head_element("meta", &props.attributes(), None);
    }

    fn create_script(&self, props: dioxus::document::ScriptProps) {
        self.create_head_element("script", &props.attributes(), props.script_contents().ok());
    }

    fn create_style(&self, props: dioxus::document::StyleProps) {
        self.create_head_element("style", &props.attributes(), props.style_contents().ok());
    }

    fn create_link(&self, props: dioxus::document::LinkProps) {
        self.create_head_element("link", &props.attributes(), None);
    }

    fn create_head_component(&self) -> bool {
        true
    }
}

// Renderer and wgpu state
mod standalone_renderer {
    use super::*;
    use anyrender_vello::VelloScenePainter;
    use blitz_paint::paint_scene;
    use vello::{
        peniko::color::AlphaColor, RenderParams, Renderer as VelloRenderer, RendererOptions, Scene,
    };
    use wgpu::util::TextureBlitter;

    pub struct WgpuState {
        pub instance: wgpu::Instance,
        pub surface: wgpu::Surface<'static>,
        pub device: Arc<wgpu::Device>,
        pub queue: Arc<wgpu::Queue>,
        pub config: wgpu::SurfaceConfiguration,
    }

    impl WgpuState {
        pub fn new(
            window_handle: RawWindowHandle,
            display_handle: RawDisplayHandle,
            width: u32,
            height: u32,
        ) -> Self {
            use pollster::FutureExt;

            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });

            struct RawHandleWrapper {
                window: RawWindowHandle,
                display: RawDisplayHandle,
            }
            impl raw_window_handle::HasWindowHandle for RawHandleWrapper {
                fn window_handle(
                    &self,
                ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
                {
                    Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.window) })
                }
            }
            impl raw_window_handle::HasDisplayHandle for RawHandleWrapper {
                fn display_handle(
                    &self,
                ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
                {
                    Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(self.display) })
                }
            }

            let wrapper = RawHandleWrapper {
                window: window_handle,
                display: display_handle,
            };

            let surface = unsafe {
                instance
                    .create_surface_unsafe(
                        wgpu::SurfaceTargetUnsafe::from_window(&wrapper)
                            .expect("Failed to create surface target"),
                    )
                    .expect("Failed to create surface")
            };

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: Some(&surface),
                })
                .block_on()
                .expect("Failed to find an appropriate adapter");

            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("standalone device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
                .block_on()
                .expect("Failed to create device");

            let device = Arc::new(device);
            let queue = Arc::new(queue);

            let surface_caps = surface.get_capabilities(&adapter);
            // IMPORTANT: We must use a NON-sRGB format here!
            // Vello renders to Rgba8Unorm (linear), and CSS colors are specified in sRGB.
            // Blitz/Vello handles the sRGB->linear conversion internally, so the output
            // from Vello is already in the correct color space for display.
            // If we use an sRGB surface format, the GPU applies an additional gamma curve
            // which makes dark colors appear lighter (gamma applied twice).
            let format = surface_caps
                .formats
                .iter()
                .find(|f| !f.is_srgb()) // Prefer non-sRGB format
                .copied()
                .unwrap_or(surface_caps.formats[0]);

            println!("Surface format: {:?} (sRGB: {})", format, format.is_srgb());

            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: width.max(1),
                height: height.max(1),
                present_mode: wgpu::PresentMode::AutoVsync,
                alpha_mode: surface_caps.alpha_modes[0],
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };

            surface.configure(&device, &config);

            Self {
                instance,
                surface,
                device,
                queue,
                config,
            }
        }

        pub fn resize(&mut self, width: u32, height: u32) {
            if width > 0 && height > 0 {
                self.config.width = width;
                self.config.height = height;
                self.surface.configure(&self.device, &self.config);
            }
        }

        pub fn format(&self) -> wgpu::TextureFormat {
            self.config.format
        }
    }

    pub struct Renderer {
        vello_renderer: VelloRenderer,
        scene: Scene,
        target_texture: Option<wgpu::Texture>,
        target_view: Option<wgpu::TextureView>,
        blitter: Option<TextureBlitter>,
        last_width: u32,
        last_height: u32,
    }

    impl Renderer {
        pub fn new(device: &wgpu::Device) -> Self {
            let vello_renderer = VelloRenderer::new(
                device,
                RendererOptions {
                    use_cpu: false,
                    antialiasing_support: vello::AaSupport::all(),
                    num_init_threads: None,
                    pipeline_cache: None,
                },
            )
            .expect("Failed to create Vello renderer");

            Self {
                vello_renderer,
                scene: Scene::new(),
                target_texture: None,
                target_view: None,
                blitter: None,
                last_width: 0,
                last_height: 0,
            }
        }

        fn ensure_target(
            &mut self,
            device: &wgpu::Device,
            surface_format: wgpu::TextureFormat,
            width: u32,
            height: u32,
        ) {
            if self.last_width != width
                || self.last_height != height
                || self.target_texture.is_none()
            {
                let target_texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("vello target"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_formats: &[],
                });
                let target_view =
                    target_texture.create_view(&wgpu::TextureViewDescriptor::default());
                let blitter = TextureBlitter::new(device, surface_format);

                self.target_texture = Some(target_texture);
                self.target_view = Some(target_view);
                self.blitter = Some(blitter);
                self.last_width = width;
                self.last_height = height;
            }
        }

        pub fn render(
            &mut self,
            wgpu_state: &WgpuState,
            doc: &DioxusDocument,
            scale: f32,
            width: u32,
            height: u32,
        ) {
            let frame = match wgpu_state.surface.get_current_texture() {
                Ok(frame) => frame,
                Err(wgpu::SurfaceError::Outdated) => return,
                Err(e) => {
                    eprintln!("Failed to get next frame: {:?}", e);
                    return;
                }
            };

            self.ensure_target(&wgpu_state.device, wgpu_state.format(), width, height);

            let target_view = self.target_view.as_ref().expect("Target view not created");
            let blitter = self.blitter.as_ref().expect("Blitter not created");

            self.scene.reset();
            paint_scene(
                &mut VelloScenePainter::new(&mut self.scene),
                doc,
                scale as f64,
                width,
                height,
            );

            self.vello_renderer
                .render_to_texture(
                    &wgpu_state.device,
                    &wgpu_state.queue,
                    &self.scene,
                    target_view,
                    &RenderParams {
                        base_color: AlphaColor::TRANSPARENT,
                        width,
                        height,
                        antialiasing_method: vello::AaConfig::Msaa16,
                    },
                )
                .expect("Failed to render");

            let surface_view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder =
                wgpu_state
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("blit encoder"),
                    });

            blitter.copy(&wgpu_state.device, &mut encoder, target_view, &surface_view);

            wgpu_state.queue.submit(std::iter::once(encoder.finish()));
            frame.present();
        }
    }
}

use standalone_renderer::{Renderer, WgpuState};

/// Helper component for switch rows
#[component]
fn SwitchRow(label: &'static str, default_on: bool) -> Element {
    let checked = use_signal(|| default_on);

    rsx! {
        div {
            class: "flex items-center justify-between",
            span { class: "text-sm text-foreground", "{label}" }
            Switch {
                checked: checked,
            }
        }
    }
}

/// Resize handle component - renders a draggable corner handle
///
/// This component uses window-level mouse tracking for drag-to-resize,
/// which allows resizing to work even when dragging outside the window bounds.
#[allow(non_snake_case)]
#[component]
fn ResizeHandle(
    /// Minimum window width
    #[props(default = 300.0)]
    min_width: f64,
    /// Minimum window height
    #[props(default = 200.0)]
    min_height: f64,
) -> Element {
    let proxy = use_window_proxy();

    // Track if we're dragging (for visual feedback only - actual drag state is in window handler)
    let mut is_dragging = use_signal(|| false);

    let handle_class = if is_dragging() {
        "resize-handle dragging"
    } else {
        "resize-handle"
    };

    rsx! {
        // Resize handle in the corner
        div {
            class: "{handle_class}",

            onmousedown: move |evt| {
                evt.stop_propagation();
                let coords = evt.client_coordinates();
                println!("Resize handle clicked at ({}, {})", coords.x, coords.y);

                // Tell the window handler to start tracking mouse globally
                proxy.start_resize_drag(min_width, min_height);
                is_dragging.set(true);
            },

            // The visible triangle
            div {
                class: "resize-handle-triangle",
            }
        }
    }
}

/// Main application component - simplified for resize testing
#[allow(non_snake_case)]
fn App() -> Element {
    // Additional CSS to fix common issues and resize handle
    let extra_css = r#"
        /* Ensure html/body have no margins and proper background */
        html, body {
            margin: 0;
            padding: 0;
            min-height: 100%;
            background-color: var(--background);
        }
        /* Resize handle styles */
        .resize-handle {
            position: fixed;
            bottom: 0;
            right: 0;
            width: 24px;
            height: 24px;
            cursor: nwse-resize;
            z-index: 9999;
            background: transparent;
        }
        .resize-handle-triangle {
            position: absolute;
            bottom: 3px;
            right: 3px;
            width: 0;
            height: 0;
            border-style: solid;
            border-width: 0 0 14px 14px;
            border-color: transparent transparent #888888 transparent;
        }
        .resize-handle:hover .resize-handle-triangle,
        .resize-handle.dragging .resize-handle-triangle {
            border-color: transparent transparent #cccccc transparent;
        }
    "#;

    rsx! {
        // Inject Tailwind CSS using inline style element (not document::Style)
        // dioxus-native/Blitz processes inline style elements directly
        style { {THEME_CSS} }
        // Inject extra fixes
        style { {extra_css} }

        // Main container with dark theme (no min-h-screen to allow small windows)
        div {
            class: "dark bg-background text-foreground min-h-full",

            // Content wrapper with padding
            div {
                class: "flex flex-col items-center px-6 py-8 gap-6 max-w-lg mx-auto",

                // Header
                h1 {
                    class: "text-3xl font-bold text-foreground text-center",
                    "Window Resize Test"
                }

                // ═══════════════════════════════════════════
                // SECTION: Window Resize Test
                // ═══════════════════════════════════════════
                div {
                    class: "bg-card rounded-xl p-5 w-full shadow-lg border border-border",

                    h2 {
                        class: "text-base font-semibold text-foreground mb-4",
                        "Programmatic Resize"
                    }

                    {
                        // Capture the window proxy outside the closures
                        let proxy = use_window_proxy();
                        rsx! {
                            div {
                                class: "flex gap-2 flex-wrap justify-center",
                                Button {
                                    variant: ButtonVariant::Outline,
                                    size: ButtonSize::Small,
                                    on_click: {
                                        let proxy = proxy.clone();
                                        move |_| {
                                            println!("Button clicked: 400x300");
                                            proxy.resize_window(400.0, 300.0);
                                        }
                                    },
                                    "400x300"
                                }
                                Button {
                                    variant: ButtonVariant::Outline,
                                    size: ButtonSize::Small,
                                    on_click: {
                                        let proxy = proxy.clone();
                                        move |_| {
                                            println!("Button clicked: 500x400");
                                            proxy.resize_window(500.0, 400.0);
                                        }
                                    },
                                    "500x400"
                                }
                                Button {
                                    variant: ButtonVariant::Primary,
                                    size: ButtonSize::Small,
                                    on_click: {
                                        let proxy = proxy.clone();
                                        move |_| {
                                            println!("Button clicked: 600x500");
                                            proxy.resize_window(600.0, 500.0);
                                        }
                                    },
                                    "600x500"
                                }
                                Button {
                                    variant: ButtonVariant::Outline,
                                    size: ButtonSize::Small,
                                    on_click: {
                                        let proxy = proxy.clone();
                                        move |_| {
                                            println!("Button clicked: 800x600");
                                            proxy.resize_window(800.0, 600.0);
                                        }
                                    },
                                    "800x600"
                                }
                            }
                        }
                    }

                    p {
                        class: "text-xs text-muted-foreground text-center mt-3",
                        "Click buttons or drag the corner handle to resize"
                    }
                }

                // Simple counter to verify interactivity still works
                div {
                    class: "bg-card rounded-xl p-5 w-full shadow-lg border border-border",

                    h2 {
                        class: "text-base font-semibold text-foreground mb-3",
                        "Counter Test"
                    }

                    {
                        let mut count = use_signal(|| 0i32);
                        rsx! {
                            div {
                                class: "text-4xl font-bold text-center py-2 font-mono",
                                style: "color: var(--primary);",
                                "{count}"
                            }
                            div {
                                class: "flex gap-2 justify-center",
                                Button {
                                    variant: ButtonVariant::Primary,
                                    on_click: move |_| count += 1,
                                    "+"
                                }
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    on_click: move |_| count -= 1,
                                    "-"
                                }
                            }
                        }
                    }
                }

                // Footer
                p {
                    class: "text-xs text-muted-foreground text-center mt-2",
                    "Rendered with Vello + wgpu via baseview"
                }
            }

            // Resize handle in bottom-right corner
            ResizeHandle {
                min_width: 350.0,
                min_height: 250.0,
            }
        }
    }
}

/// Standalone window handler
struct StandaloneHandler {
    dioxus_doc: Option<DioxusDocument>,
    wgpu_state: Option<WgpuState>,
    renderer: Option<Renderer>,
    animation_start: Instant,
    needs_redraw: Arc<AtomicBool>,
    mouse_pos: (f32, f32),
    mouse_buttons: MouseEventButtons,
    width: u32,
    height: u32,
    scale_factor: f32,
    window_handle: Option<RawWindowHandle>,
    display_handle: Option<RawDisplayHandle>,
    message_receiver: Option<Receiver<WindowMessage>>,
    /// Whether we're in resize-drag mode (tracking mouse globally)
    resize_dragging: bool,
    /// Minimum size constraints during resize drag
    resize_min: (f64, f64),
}

impl StandaloneHandler {
    fn new(window: &mut baseview::Window) -> Self {
        // Use the window size from WindowOpenOptions (500x800 logical)
        // We'll get the actual physical size and scale from the first Resized event
        // For now, estimate based on typical 2x Retina scale
        let logical_width = 500.0;
        let logical_height = 400.0;
        let scale_factor = 2.0_f32; // Typical macOS Retina scale
        let width = (logical_width * scale_factor as f64) as u32;
        let height = (logical_height * scale_factor as f64) as u32;

        println!(
            "Window created: {}x{} logical, {}x{} physical @ {}x scale",
            logical_width, logical_height, width, height, scale_factor
        );

        // Get raw handles using raw-window-handle 0.5 traits and convert to 0.6 types
        let window_handle = convert_window_handle(window.raw_window_handle());
        let display_handle = convert_display_handle(window.raw_display_handle());

        Self {
            dioxus_doc: None,
            wgpu_state: None,
            renderer: None,
            animation_start: Instant::now(),
            needs_redraw: Arc::new(AtomicBool::new(true)),
            mouse_pos: (0.0, 0.0),
            mouse_buttons: MouseEventButtons::empty(),
            width,
            height,
            scale_factor,
            window_handle,
            display_handle,
            message_receiver: None,
            resize_dragging: false,
            resize_min: (300.0, 200.0),
        }
    }

    fn initialize(&mut self) {
        let (Some(window_handle), Some(display_handle)) = (self.window_handle, self.display_handle)
        else {
            eprintln!("Cannot initialize: missing window handles");
            return;
        };

        println!("Initializing wgpu...");
        let wgpu_state = WgpuState::new(window_handle, display_handle, self.width, self.height);

        println!("Creating renderer...");
        let renderer = Renderer::new(&wgpu_state.device);

        println!("Creating Dioxus document...");
        let vdom = VirtualDom::new(App);

        let viewport = Viewport::new(
            self.width,
            self.height,
            self.scale_factor,
            ColorScheme::Dark,
        );

        // Create document with default stylesheet
        // Tailwind CSS will be injected via a <style> element in the App component
        let mut dioxus_doc = DioxusDocument::new(
            vdom,
            DocumentConfig {
                viewport: Some(viewport),
                ..Default::default()
            },
        );

        // Create channel for window messages (document::Style, resize requests, etc.)
        let (sender, receiver) = unbounded();

        // Setup WindowProxy to handle document operations and window resize
        // This must be done BEFORE initial_build() so that document::Style works
        let proxy = WindowProxy::new(sender);
        let proxy_for_doc = Rc::new(proxy.clone());
        dioxus_doc.vdom.in_scope(ScopeId::ROOT, move || {
            // Provide as Document for document::Style etc.
            provide_context(proxy_for_doc as Rc<dyn dioxus::document::Document>);
            // Also provide as WindowProxy for resize operations
            provide_context(proxy);
        });

        dioxus_doc.initial_build();

        // Process any messages that were queued during initial_build()
        // This is critical for document::Style to work - the CSS must be added
        // to the stylist BEFORE the first resolve() call
        while let Ok(msg) = receiver.try_recv() {
            match msg {
                WindowMessage::CreateHeadElement {
                    name,
                    attributes,
                    contents,
                } => {
                    println!(
                        "Processing head element during init: {} ({} bytes)",
                        name,
                        contents.as_ref().map(|c| c.len()).unwrap_or(0)
                    );
                    let attrs: Vec<(String, String)> = attributes;
                    dioxus_doc.create_head_element(&name, &attrs, &contents);
                }
                WindowMessage::ResizeWindow { .. }
                | WindowMessage::StartResizeDrag { .. }
                | WindowMessage::StopResizeDrag => {
                    // Resize requests during init are ignored
                }
            }
        }

        // Store receiver for future message processing
        self.message_receiver = Some(receiver);

        dioxus_doc.resolve(0.0);

        self.wgpu_state = Some(wgpu_state);
        self.renderer = Some(renderer);
        self.dioxus_doc = Some(dioxus_doc);

        println!("Initialization complete!");
    }
}

impl WindowHandler for StandaloneHandler {
    fn on_frame(&mut self, window: &mut baseview::Window) {
        if self.wgpu_state.is_none() {
            self.initialize();
        }

        let animation_time = self.animation_start.elapsed().as_secs_f64();
        let needs_redraw = self.needs_redraw.clone();

        // Collect messages first to avoid borrow conflicts
        let mut pending_resize: Option<(f64, f64)> = None;
        let mut head_elements: Vec<(String, Vec<(String, String)>, Option<String>)> = Vec::new();
        let mut start_drag: Option<(f64, f64)> = None;
        let mut stop_drag = false;

        if let Some(receiver) = &self.message_receiver {
            while let Ok(msg) = receiver.try_recv() {
                match msg {
                    WindowMessage::CreateHeadElement {
                        name,
                        attributes,
                        contents,
                    } => {
                        head_elements.push((name, attributes, contents));
                    }
                    WindowMessage::ResizeWindow { width, height } => {
                        pending_resize = Some((width, height));
                    }
                    WindowMessage::StartResizeDrag {
                        min_width,
                        min_height,
                    } => {
                        start_drag = Some((min_width, min_height));
                    }
                    WindowMessage::StopResizeDrag => {
                        stop_drag = true;
                    }
                }
            }
        }

        // Handle drag state changes
        if let Some((min_w, min_h)) = start_drag {
            self.resize_dragging = true;
            self.resize_min = (min_w, min_h);
            println!(">>> Resize drag started (min: {}x{})", min_w, min_h);
        }
        if stop_drag {
            self.resize_dragging = false;
            println!(">>> Resize drag stopped");
        }

        // Handle resize if requested
        if let Some((width, height)) = pending_resize {
            println!(">>> Resize requested: {}x{}", width, height);

            // Call baseview's window.resize()
            window.resize(baseview::Size::new(width, height));

            // baseview's resize() doesn't trigger a Resized event on macOS,
            // so we need to manually update our state
            let new_width = (width * self.scale_factor as f64) as u32;
            let new_height = (height * self.scale_factor as f64) as u32;

            println!(">>> Manual resize: {}x{} physical", new_width, new_height);

            self.width = new_width;
            self.height = new_height;

            if let Some(doc) = &mut self.dioxus_doc {
                doc.set_viewport(Viewport::new(
                    self.width,
                    self.height,
                    self.scale_factor,
                    ColorScheme::Dark,
                ));
                doc.resolve(animation_time);
            }
            if let Some(wgpu_state) = &mut self.wgpu_state {
                wgpu_state.resize(self.width, self.height);
            }
            self.needs_redraw.store(true, Ordering::Relaxed);
            println!(">>> Resize complete");
        }

        let scale_factor = self.scale_factor;
        let width = self.width;
        let height = self.height;

        let Some(doc) = &mut self.dioxus_doc else {
            return;
        };
        let Some(wgpu_state) = &self.wgpu_state else {
            return;
        };
        let Some(renderer) = &mut self.renderer else {
            return;
        };

        // Process head element messages
        for (name, attributes, contents) in head_elements {
            println!(
                "Creating head element: {} (contents: {} bytes)",
                name,
                contents.as_ref().map(|c| c.len()).unwrap_or(0)
            );
            let attrs: Vec<(String, String)> = attributes;
            doc.create_head_element(&name, &attrs, &contents);
        }

        // Create waker
        struct RedrawWaker(Arc<AtomicBool>);
        impl ArcWake for RedrawWaker {
            fn wake_by_ref(arc_self: &Arc<Self>) {
                arc_self.0.store(true, Ordering::Relaxed);
            }
        }
        let waker = futures_util::task::waker(Arc::new(RedrawWaker(needs_redraw.clone())));
        let cx = std::task::Context::from_waker(&waker);
        doc.poll(Some(cx));

        doc.resolve(animation_time);
        renderer.render(wgpu_state, doc, scale_factor, width, height);
        self.needs_redraw.store(false, Ordering::Relaxed);
    }

    fn on_event(
        &mut self,
        window: &mut baseview::Window,
        event: baseview::Event,
    ) -> baseview::EventStatus {
        use baseview::{Event, MouseEvent, WindowEvent};
        use blitz_traits::events::{BlitzMouseButtonEvent, MouseEventButton, UiEvent};

        match &event {
            Event::Window(WindowEvent::Resized(info)) => {
                let new_width = info.physical_size().width as u32;
                let new_height = info.physical_size().height as u32;
                let new_scale = info.scale() as f32;

                println!(
                    ">>> Window resized: {}x{} physical, scale={}, logical={}x{}",
                    new_width,
                    new_height,
                    new_scale,
                    info.logical_size().width,
                    info.logical_size().height
                );

                self.width = new_width;
                self.height = new_height;
                self.scale_factor = new_scale;

                if let Some(doc) = &mut self.dioxus_doc {
                    println!(">>> Setting viewport and resolving layout...");
                    doc.set_viewport(Viewport::new(
                        self.width,
                        self.height,
                        self.scale_factor,
                        ColorScheme::Dark,
                    ));
                    // Force a re-layout after viewport change
                    doc.resolve(self.animation_start.elapsed().as_secs_f64());
                    println!(">>> Layout resolved");
                }
                if let Some(wgpu_state) = &mut self.wgpu_state {
                    println!(">>> Resizing wgpu surface...");
                    wgpu_state.resize(self.width, self.height);
                }
                self.needs_redraw.store(true, Ordering::Relaxed);
                println!(">>> Resize complete");
                return baseview::EventStatus::Captured;
            }
            Event::Mouse(mouse_event) => {
                // Handle resize dragging at window level (works even when mouse is outside window bounds)
                if self.resize_dragging {
                    match mouse_event {
                        MouseEvent::CursorMoved { position, .. } => {
                            // Mouse position is the new window size (we're dragging bottom-right corner)
                            // Convert from physical to logical coordinates
                            let new_width = (position.x as f64 / self.scale_factor as f64 + 12.0)
                                .max(self.resize_min.0);
                            let new_height = (position.y as f64 / self.scale_factor as f64 + 12.0)
                                .max(self.resize_min.1);

                            // Only resize if changed significantly
                            let cur_width = self.width as f64 / self.scale_factor as f64;
                            let cur_height = self.height as f64 / self.scale_factor as f64;

                            if (new_width - cur_width).abs() >= 2.0
                                || (new_height - cur_height).abs() >= 2.0
                            {
                                window.resize(baseview::Size::new(new_width, new_height));

                                // Manually update state since baseview doesn't fire resize event
                                let phys_w = (new_width * self.scale_factor as f64) as u32;
                                let phys_h = (new_height * self.scale_factor as f64) as u32;
                                self.width = phys_w;
                                self.height = phys_h;

                                if let Some(doc) = &mut self.dioxus_doc {
                                    doc.set_viewport(Viewport::new(
                                        phys_w,
                                        phys_h,
                                        self.scale_factor,
                                        ColorScheme::Dark,
                                    ));
                                    doc.resolve(self.animation_start.elapsed().as_secs_f64());
                                }
                                if let Some(wgpu_state) = &mut self.wgpu_state {
                                    wgpu_state.resize(phys_w, phys_h);
                                }
                                self.needs_redraw.store(true, Ordering::Relaxed);
                            }
                            return baseview::EventStatus::Captured;
                        }
                        MouseEvent::ButtonReleased {
                            button: baseview::MouseButton::Left,
                            ..
                        } => {
                            self.resize_dragging = false;
                            println!(">>> Resize drag stopped (mouse up)");
                            // Don't return - let the event propagate to Dioxus too
                        }
                        _ => {}
                    }
                }

                // Update mouse position
                if let MouseEvent::CursorMoved { position, .. } = mouse_event {
                    self.mouse_pos.0 = position.x as f32;
                    self.mouse_pos.1 = position.y as f32;
                }

                if let Some(doc) = &mut self.dioxus_doc {
                    let ui_event = match mouse_event {
                        MouseEvent::CursorMoved { position, .. } => {
                            // Already updated mouse_pos above
                            Some(UiEvent::MouseMove(BlitzMouseButtonEvent {
                                x: position.x as f32,
                                y: position.y as f32,
                                button: MouseEventButton::Main,
                                buttons: self.mouse_buttons,
                                mods: dioxus::prelude::Modifiers::empty(),
                            }))
                        }
                        MouseEvent::ButtonPressed { button, .. } => {
                            let btn = match button {
                                baseview::MouseButton::Left => MouseEventButton::Main,
                                baseview::MouseButton::Right => MouseEventButton::Secondary,
                                baseview::MouseButton::Middle => MouseEventButton::Auxiliary,
                                _ => MouseEventButton::Main,
                            };
                            self.mouse_buttons |= MouseEventButtons::from(btn);
                            Some(UiEvent::MouseDown(BlitzMouseButtonEvent {
                                x: self.mouse_pos.0,
                                y: self.mouse_pos.1,
                                button: btn,
                                buttons: self.mouse_buttons,
                                mods: dioxus::prelude::Modifiers::empty(),
                            }))
                        }
                        MouseEvent::ButtonReleased { button, .. } => {
                            let btn = match button {
                                baseview::MouseButton::Left => MouseEventButton::Main,
                                baseview::MouseButton::Right => MouseEventButton::Secondary,
                                baseview::MouseButton::Middle => MouseEventButton::Auxiliary,
                                _ => MouseEventButton::Main,
                            };
                            self.mouse_buttons &= !MouseEventButtons::from(btn);
                            Some(UiEvent::MouseUp(BlitzMouseButtonEvent {
                                x: self.mouse_pos.0,
                                y: self.mouse_pos.1,
                                button: btn,
                                buttons: self.mouse_buttons,
                                mods: dioxus::prelude::Modifiers::empty(),
                            }))
                        }
                        MouseEvent::WheelScrolled { .. } => {
                            // TODO: Scroll handling - requires newer blitz-traits version
                            None
                        }
                        _ => None,
                    };

                    if let Some(evt) = ui_event {
                        doc.handle_ui_event(evt);
                        self.needs_redraw.store(true, Ordering::Relaxed);
                        return baseview::EventStatus::Captured;
                    }
                }
            }
            _ => {}
        }

        baseview::EventStatus::Ignored
    }
}

/// Convert raw-window-handle 0.5 to 0.6 window handle
fn convert_window_handle(handle: raw_window_handle_05::RawWindowHandle) -> Option<RawWindowHandle> {
    use std::ptr::NonNull;

    match handle {
        #[cfg(target_os = "macos")]
        raw_window_handle_05::RawWindowHandle::AppKit(h) => {
            let ns_view = NonNull::new(h.ns_view as *mut std::ffi::c_void)?;
            let handle = raw_window_handle::AppKitWindowHandle::new(ns_view);
            Some(RawWindowHandle::AppKit(handle))
        }
        #[cfg(target_os = "windows")]
        raw_window_handle_05::RawWindowHandle::Win32(h) => {
            use std::num::NonZeroIsize;
            let handle =
                raw_window_handle::Win32WindowHandle::new(NonZeroIsize::new(h.hwnd as isize)?);
            Some(RawWindowHandle::Win32(handle))
        }
        #[cfg(target_os = "linux")]
        raw_window_handle_05::RawWindowHandle::Xcb(h) => {
            use std::num::NonZeroIsize;
            let handle =
                raw_window_handle::XcbWindowHandle::new(NonZeroIsize::new(h.window as isize)?);
            Some(RawWindowHandle::Xcb(handle))
        }
        #[cfg(target_os = "linux")]
        raw_window_handle_05::RawWindowHandle::Xlib(h) => {
            let handle = raw_window_handle::XlibWindowHandle::new(h.window as u32);
            Some(RawWindowHandle::Xlib(handle))
        }
        _ => None,
    }
}

/// Convert raw-window-handle 0.5 to 0.6 display handle
fn convert_display_handle(
    handle: raw_window_handle_05::RawDisplayHandle,
) -> Option<RawDisplayHandle> {
    use std::ptr::NonNull;

    match handle {
        #[cfg(target_os = "macos")]
        raw_window_handle_05::RawDisplayHandle::AppKit(_) => Some(RawDisplayHandle::AppKit(
            raw_window_handle::AppKitDisplayHandle::new(),
        )),
        #[cfg(target_os = "windows")]
        raw_window_handle_05::RawDisplayHandle::Windows(_) => Some(RawDisplayHandle::Windows(
            raw_window_handle::WindowsDisplayHandle::new(),
        )),
        #[cfg(target_os = "linux")]
        raw_window_handle_05::RawDisplayHandle::Xcb(h) => {
            let connection = NonNull::new(h.connection as *mut _);
            let handle = raw_window_handle::XcbDisplayHandle::new(connection, h.screen);
            Some(RawDisplayHandle::Xcb(handle))
        }
        #[cfg(target_os = "linux")]
        raw_window_handle_05::RawDisplayHandle::Xlib(h) => {
            let display = NonNull::new(h.display as *mut _);
            let handle = raw_window_handle::XlibDisplayHandle::new(display, h.screen);
            Some(RawDisplayHandle::Xlib(handle))
        }
        _ => None,
    }
}

fn main() {
    println!("Opening standalone window...");

    Window::open_blocking(
        WindowOpenOptions {
            title: "Window Resize Test".to_string(),
            size: Size::new(500.0, 400.0), // Smaller initial size for testing
            scale: WindowScalePolicy::SystemScaleFactor,
        },
        |window| StandaloneHandler::new(window),
    );

    println!("Window closed.");
}
