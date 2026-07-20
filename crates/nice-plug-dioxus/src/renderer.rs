//! Vello renderer integration.

use crate::custom_paint::{OverlayLayer, OverlayRegistry};
#[cfg(feature = "softbuffer-blit")]
use crate::wgpu_offscreen::WgpuOffscreenState;
use crate::wgpu_state::WgpuState;
use anyrender_vello::VelloScenePainter;
use blitz_dom::Document as _;
use blitz_paint::paint_scene;
use dioxus_native::DioxusDocument;
use vello::{
    peniko::{color::AlphaColor, kurbo::Affine},
    RenderParams, Renderer as VelloRenderer, RendererOptions, Scene,
};
use wgpu::util::TextureBlitter;

/// A scene overlay that gets composited on top of the Dioxus DOM scene.
///
/// Paint sources produce vello `Scene` content each frame. The renderer appends these
/// to the main scene after Dioxus DOM painting, allowing GPU-rendered content
/// (like EQ graphs, spectrum analyzers) to appear within the Dioxus layout.
pub trait SceneOverlay: 'static {
    /// Paint the overlay into the given scene.
    ///
    /// * `transform` — affine transform mapping element-local CSS coords to window
    ///   physical coords. Pass this to `scene.fill()` / `scene.stroke()`.
    /// * `width` / `height` — overlay dimensions in CSS pixels.
    /// * `scale` — display scale factor.
    fn paint(&mut self, scene: &mut Scene, transform: Affine, width: u32, height: u32, scale: f64);
}

/// Manages Vello rendering to a wgpu surface.
pub struct Renderer {
    vello_renderer: VelloRenderer,
    scene: Scene,
    // Intermediate texture for Vello rendering (compute shader output, must be Rgba8Unorm)
    target_texture: Option<wgpu::Texture>,
    /// View for Vello to render into (linear Rgba8Unorm for compute shader)
    target_view: Option<wgpu::TextureView>,
    blitter: Option<TextureBlitter>,
    last_width: u32,
    last_height: u32,
    /// Shared overlay registry — also provided as Dioxus context for `use_scene_overlay`.
    overlay_registry: OverlayRegistry,
    overlay_scene: Scene,
}

impl Renderer {
    /// Create a new renderer.
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
            overlay_registry: OverlayRegistry::new(),
            overlay_scene: Scene::new(),
        }
    }

    /// Get a clone of the overlay registry for providing as Dioxus context.
    pub fn overlay_registry(&self) -> OverlayRegistry {
        self.overlay_registry.clone()
    }

    /// Ensure the intermediate texture is the right size.
    fn ensure_target(
        &mut self,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) {
        if self.last_width != width || self.last_height != height || self.target_texture.is_none() {
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
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                format: wgpu::TextureFormat::Rgba8Unorm,
                view_formats: &[],
            });

            let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let blitter = TextureBlitter::new(device, surface_format);

            self.target_texture = Some(target_texture);
            self.target_view = Some(target_view);
            self.blitter = Some(blitter);
            self.last_width = width;
            self.last_height = height;
        }
    }

    /// Render the document to the surface.
    pub fn render(
        &mut self,
        wgpu_state: &WgpuState,
        doc: &mut DioxusDocument,
        scale: f32,
        width: u32,
        height: u32,
    ) {
        if !wgpu_state.is_configured() {
            return;
        }

        // wgpu 29: `get_current_texture()` returns the `CurrentSurfaceTexture` enum
        // (no longer a `Result`). Use the frame on success/suboptimal, skip otherwise.
        let frame = match wgpu_state.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            _ => return,
        };

        self.ensure_target(&wgpu_state.device, wgpu_state.format(), width, height);

        let target_view = self.target_view.as_ref().expect("Target view not created");
        let blitter = self.blitter.as_ref().expect("Blitter not created");

        // Paint background overlays (behind DOM — e.g. waveform backgrounds)
        self.scene.reset();
        self.overlay_scene.reset();
        self.overlay_registry.paint_layer(
            &mut self.overlay_scene,
            width,
            height,
            scale as f64,
            Some(OverlayLayer::Background),
        );
        self.scene.append(&self.overlay_scene, None);

        // Paint the Dioxus DOM
        paint_scene(
            &mut VelloScenePainter::new(&mut self.scene),
            &mut doc.inner_mut(),
            scale as f64,
            width,
            height,
            0,
            0,
        );

        // Paint foreground overlays (on top of DOM — e.g. EQ graphs)
        self.overlay_scene.reset();
        self.overlay_registry.paint_layer(
            &mut self.overlay_scene,
            width,
            height,
            scale as f64,
            Some(OverlayLayer::Foreground),
        );
        self.scene.append(&self.overlay_scene, None);

        // Render to intermediate texture
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
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("Failed to render");

        // Blit to surface
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

    /// Render the document to an offscreen texture (for softbuffer blit).
    #[cfg(feature = "softbuffer-blit")]
    pub fn render_offscreen(
        &mut self,
        wgpu_state: &WgpuOffscreenState,
        doc: &mut DioxusDocument,
        scale: f32,
        width: u32,
        height: u32,
    ) {
        self.ensure_target(&wgpu_state.device, wgpu_state.format(), width, height);

        let target_view = self.target_view.as_ref().expect("Target view not created");
        let blitter = self.blitter.as_ref().expect("Blitter not created");

        // Paint background overlays (behind DOM)
        self.scene.reset();
        self.overlay_scene.reset();
        self.overlay_registry.paint_layer(
            &mut self.overlay_scene,
            width,
            height,
            scale as f64,
            Some(OverlayLayer::Background),
        );
        self.scene.append(&self.overlay_scene, None);

        // Paint DOM
        paint_scene(
            &mut VelloScenePainter::new(&mut self.scene),
            &mut *doc.inner_mut(),
            scale as f64,
            width,
            height,
            0,
            0,
        );

        // Paint foreground overlays (on top of DOM)
        self.overlay_scene.reset();
        self.overlay_registry.paint_layer(
            &mut self.overlay_scene,
            width,
            height,
            scale as f64,
            Some(OverlayLayer::Foreground),
        );
        self.scene.append(&self.overlay_scene, None);

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
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("Failed to render");

        let mut encoder =
            wgpu_state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("offscreen blit encoder"),
                });

        blitter.copy(
            &wgpu_state.device,
            &mut encoder,
            target_view,
            &wgpu_state.render_texture_view,
        );

        wgpu_state.queue.submit(std::iter::once(encoder.finish()));
    }
}
