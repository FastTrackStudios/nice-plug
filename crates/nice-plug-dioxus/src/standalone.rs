//! Standalone window support for testing Dioxus editors outside a DAW.
//!
//! This module provides:
//! - `open_standalone` / `open_standalone_with_state`: windowed rendering for interactive testing
//!   (uses baseview, fully featured including VelloCanvas overlays)
//! - `launch_native_app_with_state`: launch via dioxus_native (winit, supports `dx serve`)
//!   VelloCanvas overlays are no-op in this mode — layout and CSS UI work.
//! - `open_parented_x11`: parented X11 window (simulates DAW embedding)
//! - `render_screenshot`: headless offscreen rendering to RGBA pixels for automated testing

use crate::context::ParamContext;
use crate::custom_paint::OverlayRegistry;
use crate::state::DioxusState;
use crate::SharedState;
use anyrender_vello::VelloScenePainter;
use baseview::{Size, Window, WindowOpenOptions, WindowScalePolicy};
use blitz_dom::{Document as _, DocumentConfig};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use crossbeam::channel::unbounded;
use dioxus_native::prelude::*;
use dioxus_native::DioxusDocument;
use futures_util::task::ArcWake;
use nice_plug_core::context::gui::GuiContext;
use nice_plug_core::context::PluginApi;
use nice_plug_core::params::internals::ParamPtr;
use nice_plug_core::plugin::PluginState;
use pollster::FutureExt;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use vello::{
    peniko::color::AlphaColor, RenderParams, Renderer as VelloRenderer, RendererOptions, Scene,
};

/// A no-op GuiContext for standalone testing.
/// Parameter automation and state are not available outside a DAW.
struct StandaloneGuiContext;

impl GuiContext for StandaloneGuiContext {
    fn plugin_api(&self) -> PluginApi {
        PluginApi::Clap
    }

    fn request_resize(&self) -> bool {
        true
    }

    unsafe fn raw_begin_set_parameter(&self, _param: ParamPtr) {}
    unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
        // Actually update the atomic so standalone demos and dx-serve respond to knob changes.
        unsafe { param._internal_set_normalized_value(normalized) };
    }
    unsafe fn raw_end_set_parameter(&self, _param: ParamPtr) {}

    fn get_state(&self) -> PluginState {
        PluginState {
            version: String::new(),
            params: BTreeMap::new(),
            fields: BTreeMap::new(),
        }
    }

    fn set_state(&self, _state: PluginState) {}
}

/// Open a standalone window that renders a Dioxus component using the native
/// wgpu surface path. This blocks until the window is closed.
///
/// This is useful for testing GUI rendering without a DAW host.
///
/// # Arguments
///
/// * `app` - The Dioxus component function to render
/// * `width` - Window width in logical pixels
/// * `height` - Window height in logical pixels
pub fn open_standalone(app: fn() -> Element, width: u32, height: u32) {
    open_standalone_with_state(app, width, height, None);
}

/// Open a standalone window with shared state.
pub fn open_standalone_with_state(
    app: fn() -> Element,
    width: u32,
    height: u32,
    shared_state: Option<SharedState>,
) {
    let dioxus_state = DioxusState::new(move || (width, height));
    let gui_context: Arc<dyn GuiContext> = Arc::new(StandaloneGuiContext);
    let needs_redraw = Arc::new(AtomicBool::new(true));

    Window::open_blocking(
        WindowOpenOptions {
            title: String::from("FTS GUI Test (Native wgpu Surface)"),
            size: Size::new(width as f64, height as f64),
            scale: WindowScalePolicy::ScaleFactor(1.0),
        },
        move |window| {
            // Use the native wgpu surface handler (not softbuffer)
            #[cfg(not(feature = "softbuffer-blit"))]
            {
                crate::window::DioxusWindowHandler::new_with_state(
                    window,
                    app,
                    gui_context.clone(),
                    dioxus_state.clone(),
                    needs_redraw.clone(),
                    shared_state,
                )
            }
            #[cfg(feature = "softbuffer-blit")]
            {
                crate::window_softbuffer::DioxusSoftbufferWindowHandler::new_with_state(
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
}

/// Launch a Dioxus native desktop app via the standard `dioxus_native::launch_cfg` path
/// (winit + blitz-shell). This is the entry point for `dx serve` development.
///
/// Provides a no-op `ParamContext` so knob widgets compile and mount, but
/// parameter automation is not connected. Pass `SharedState` with mock data
/// to populate UI state.
///
/// # VelloCanvas overlays
/// Vello overlays (spectrum, waveform) will be invisible because `OverlayRegistry` is
/// not connected to the blitz-shell render loop. All CSS-based UI renders normally.
pub fn launch_native_app(app: fn() -> Element, shared_state: Option<crate::SharedState>) {
    let gui_context: std::sync::Arc<dyn GuiContext> = std::sync::Arc::new(StandaloneGuiContext);
    let needs_redraw = std::sync::Arc::new(AtomicBool::new(true));
    let param_ctx = ParamContext::new(gui_context, needs_redraw);

    let mut contexts: Vec<Box<dyn Fn() -> Box<dyn std::any::Any> + Send + Sync>> =
        vec![Box::new(move || {
            Box::new(param_ctx.clone()) as Box<dyn std::any::Any>
        })];

    if let Some(state) = shared_state {
        contexts.push(Box::new(move || {
            Box::new(state.clone()) as Box<dyn std::any::Any>
        }));
    }

    dioxus_native::launch_cfg(app, contexts, vec![])
}

/// Open a Dioxus component as a child of an existing X11 window.
///
/// This simulates exactly what a DAW does: provide a parent window ID and
/// the plugin opens its GUI as a child window inside it. Uses the same
/// `open_parented` + `RwhAdapter` code path as `DioxusEditor::spawn`.
///
/// Only available with the native wgpu-surface handler (i.e. without the
/// `softbuffer-blit` feature), which is the path this helper exercises.
#[cfg(not(feature = "softbuffer-blit"))]
pub fn open_parented_x11(
    app: fn() -> Element,
    parent_window_id: u32,
    width: u32,
    height: u32,
) -> baseview::WindowHandle {
    use crate::window::DioxusWindowHandler;
    use raw_window_handle::{
        HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
        XcbDisplayHandle, XcbWindowHandle,
    };
    use std::num::NonZeroU32;

    struct X11Parent(u32);

    impl HasWindowHandle for X11Parent {
        fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, HandleError> {
            let handle = XcbWindowHandle::new(
                NonZeroU32::new(self.0).expect("X11 window ID should not be 0"),
            );
            let raw = RawWindowHandle::Xcb(handle);
            Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
        }
    }

    impl HasDisplayHandle for X11Parent {
        fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, HandleError> {
            // No display connection — same as what nih-plug provides from DAWs.
            // Baseview creates its own X11 connection internally.
            let handle = XcbDisplayHandle::new(None, 0);
            let raw = RawDisplayHandle::Xcb(handle);
            Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw) })
        }
    }

    let dioxus_state = DioxusState::new(move || (width, height));
    let gui_context: Arc<dyn GuiContext> = Arc::new(StandaloneGuiContext);
    let needs_redraw = Arc::new(AtomicBool::new(true));

    Window::open_parented(
        &X11Parent(parent_window_id),
        WindowOpenOptions {
            title: String::from("Plugin Editor (Parented Test)"),
            size: Size::new(width as f64, height as f64),
            scale: WindowScalePolicy::ScaleFactor(1.0),
        },
        move |window| {
            DioxusWindowHandler::new_with_state(
                window,
                app,
                gui_context.clone(),
                dioxus_state.clone(),
                needs_redraw.clone(),
                None,
            )
        },
    )
}

/// Document proxy for headless rendering (same as in window.rs).
#[derive(Clone)]
struct HeadlessDocProxy {
    sender: crossbeam::channel::Sender<(String, Vec<(String, String)>, Option<String>)>,
}

impl document::Document for HeadlessDocProxy {
    fn eval(&self, js: String) -> document::Eval {
        document::NoOpDocument.eval(js)
    }
    fn set_title(&self, title: String) {
        let _ = self.sender.send(("title".into(), vec![], Some(title)));
    }
    fn create_meta(&self, props: document::MetaProps) {
        let _ = self.sender.send((
            "meta".into(),
            props
                .attributes()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            None,
        ));
    }
    fn create_script(&self, props: document::ScriptProps) {
        let _ = self.sender.send((
            "script".into(),
            props
                .attributes()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            props.script_contents().ok(),
        ));
    }
    fn create_style(&self, props: document::StyleProps) {
        let _ = self.sender.send((
            "style".into(),
            props
                .attributes()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            props.style_contents().ok(),
        ));
    }
    fn create_link(&self, props: document::LinkProps) {
        let _ = self.sender.send((
            "link".into(),
            props
                .attributes()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            None,
        ));
    }
    fn create_head_component(&self) -> bool {
        true
    }
}

struct ScreenshotWaker;
impl ArcWake for ScreenshotWaker {
    fn wake_by_ref(_arc_self: &Arc<Self>) {}
}

/// Render a Dioxus component headlessly and return RGBA pixel data.
///
/// This creates an offscreen wgpu device, builds the Dioxus document,
/// resolves layout, renders via Vello, and reads pixels back.
/// No window or display connection required.
///
/// Returns RGBA8 pixel data (4 bytes per pixel, row-major).
pub fn render_screenshot(
    app: fn() -> Element,
    width: u32,
    height: u32,
    shared_state: Option<SharedState>,
) -> Vec<u8> {
    let width = width.max(1);
    let height = height.max(1);

    // --- Create offscreen wgpu device ---
    // wgpu 29: `InstanceDescriptor` no longer implements `Default`; build it explicitly.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            ..Default::default()
        })
        .block_on()
        .expect("Failed to find GPU adapter");

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("screenshot device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        })
        .block_on()
        .expect("Failed to create device");

    let device = Arc::new(device);
    let queue = Arc::new(queue);

    // --- Build the Dioxus document ---
    let vdom = VirtualDom::new(app);

    let viewport = Viewport::new(width, height, 1.0, ColorScheme::Light);
    let mut dioxus_doc = DioxusDocument::new(
        vdom,
        DocumentConfig {
            viewport: Some(viewport),
            ..Default::default()
        },
    );

    // Set up contexts
    let (doc_sender, doc_receiver) = unbounded();
    let doc_proxy = HeadlessDocProxy { sender: doc_sender };
    let doc_proxy_rc = Rc::new(doc_proxy);

    let gui_context: Arc<dyn GuiContext> = Arc::new(StandaloneGuiContext);
    let needs_redraw = Arc::new(AtomicBool::new(false));
    let param_context = ParamContext::new(gui_context, needs_redraw);
    let dioxus_state = DioxusState::new(move || (width, height));

    let overlay_registry = OverlayRegistry::new();
    let overlay_registry_for_paint = overlay_registry.clone();
    dioxus_doc.vdom.in_scope(ScopeId::ROOT, move || {
        provide_context(doc_proxy_rc as Rc<dyn document::Document>);
        provide_context(param_context);
        provide_context(dioxus_state);
        provide_context(overlay_registry);
        if let Some(state) = shared_state {
            provide_context(state);
        }
    });

    // Build and process document messages (CSS injection)
    dioxus_doc.initial_build();
    while let Ok((name, attrs, contents)) = doc_receiver.try_recv() {
        dioxus_doc.create_head_element(&name, &attrs, &contents);
    }

    // Poll VirtualDom and resolve layout.
    // Multiple poll cycles needed: onmounted fires after initial build,
    // use_effect spawns async tasks, and those need another poll to complete.
    let waker = futures_util::task::waker(Arc::new(ScreenshotWaker));
    for _ in 0..4 {
        let cx = std::task::Context::from_waker(&waker);
        dioxus_doc.poll(Some(cx));
        dioxus_doc.inner_mut().resolve(0.0);
    }

    // --- Render via Vello ---
    let mut vello_renderer = VelloRenderer::new(
        &device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::all(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("Failed to create Vello renderer");

    let mut scene = Scene::new();

    // Paint background overlays (behind DOM)
    use crate::custom_paint::OverlayLayer;
    overlay_registry_for_paint.paint_layer(
        &mut scene,
        width,
        height,
        1.0,
        Some(OverlayLayer::Background),
    );

    // Paint the DOM
    paint_scene(
        &mut VelloScenePainter::new(&mut scene),
        &mut dioxus_doc.inner_mut(),
        1.0, // scale
        width,
        height,
        0,
        0,
    );

    // Paint foreground overlays (on top of DOM)
    overlay_registry_for_paint.paint_layer(
        &mut scene,
        width,
        height,
        1.0,
        Some(OverlayLayer::Foreground),
    );

    // Vello renders to Rgba8Unorm (required for compute shaders)
    let target_format = wgpu::TextureFormat::Rgba8Unorm;
    let target_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: target_format,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());

    vello_renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &target_view,
            &RenderParams {
                base_color: AlphaColor::TRANSPARENT,
                width,
                height,
                antialiasing_method: vello::AaConfig::Msaa16,
            },
        )
        .expect("Vello render failed");

    // --- Read back pixels ---
    let bytes_per_pixel = 4u32;
    let unpadded_bpr = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bpr = unpadded_bpr.div_ceil(align) * align;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot staging"),
        size: (padded_bpr * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("screenshot readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().unwrap().unwrap();

    let data = slice.get_mapped_range();
    let result = if padded_bpr != unpadded_bpr {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * padded_bpr) as usize;
            let end = start + unpadded_bpr as usize;
            out.extend_from_slice(&data[start..end]);
        }
        out
    } else {
        data.to_vec()
    };

    drop(data);
    staging.unmap();

    result
}
