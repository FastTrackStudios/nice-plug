//! Embedded editor support for REAPER's TCP/MCP inline FX UI.
//!
//! This module provides the ability to render a Dioxus UI to REAPER's LICE bitmap
//! for embedded display in the Track Control Panel (TCP) or Mixer Control Panel (MCP).
//!
//! The implementation uses a dedicated render thread to work around the fact that
//! `DioxusDocument` is not `Send + Sync`. The render thread owns the document and
//! communicates with the main thread via channels.
//!
//! # Example
//!
//! ```ignore
//! use nice_plug_dioxus::embedded::DioxusEmbeddedEditor;
//!
//! impl Plugin for MyPlugin {
//!     fn embedded_editor(&mut self) -> Option<Arc<dyn EmbeddedEditor>> {
//!         Some(Arc::new(DioxusEmbeddedEditor::new(
//!             self.dioxus_state.clone(),
//!             App, // Your Dioxus component
//!         )))
//!     }
//! }
//! ```

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig};
use blitz_traits::shell::{ColorScheme, Viewport};
use crossbeam::channel::{bounded, Receiver, Sender};
use dioxus_native::prelude::*;
use dioxus_native::DioxusDocument;
use nice_plug_core::editor::embedded::{
    embed_flags, EmbedBitmap, EmbedContext, EmbedDrawInfo, EmbedMouseEvent, EmbedSizeHints,
    EmbeddedEditor,
};

use crate::state::DioxusState;
use crate::SharedState;

/// Commands sent to the render thread.
#[derive(Clone)]
enum RenderCommand {
    /// Request a render at the specified size.
    Render { width: u32, height: u32, scale: f32 },
    /// Mouse event to forward to the document.
    MouseEvent {
        event_type: MouseEventType,
        x: f32,
        y: f32,
        width: u32,
        height: u32,
        scale: f32,
    },
    /// Shutdown the render thread.
    Shutdown,
}

/// Mouse event types for internal use.
#[derive(Debug, Clone, Copy)]
enum MouseEventType {
    Move,
    Down,
    Up,
}

/// A rendered frame ready for display.
struct RenderedFrame {
    /// RGBA pixel buffer.
    buffer: Vec<u8>,
    /// Frame width.
    width: u32,
    /// Frame height.
    height: u32,
}

/// CPU-based embedded editor renderer using a dedicated render thread.
///
/// This approach solves the Send+Sync problem by keeping the DioxusDocument
/// on a dedicated thread and communicating via channels.
///
/// The render thread:
/// - Owns the `DioxusDocument` (which is not Send+Sync)
/// - Receives render requests via a channel
/// - Sends rendered frames back via another channel
///
/// The main thread (EmbeddedEditor methods):
/// - Sends render requests when paint() is called
/// - Receives rendered frames and caches them
/// - Copies cached frames to the LICE bitmap
pub struct DioxusEmbeddedEditor {
    /// Channel to send render commands to the render thread.
    command_tx: Sender<RenderCommand>,
    /// Channel to receive rendered frames from the render thread.
    frame_rx: Receiver<RenderedFrame>,
    /// Cached last frame for quick redraws.
    cached_frame: Mutex<Option<RenderedFrame>>,
    /// Current requested width.
    current_width: AtomicU32,
    /// Current requested height.
    current_height: AtomicU32,
    /// Flag indicating we need a fresh render.
    needs_render: AtomicBool,
    /// Handle to the render thread (stored for lifetime management).
    render_thread: Mutex<Option<JoinHandle<()>>>,
}

impl DioxusEmbeddedEditor {
    /// Create a new embedded editor.
    ///
    /// This spawns a dedicated render thread that owns the DioxusDocument.
    ///
    /// # Arguments
    ///
    /// * `_state` - Shared state with the windowed editor (for future use)
    /// * `app` - The Dioxus component function to render
    pub fn new(_state: Arc<DioxusState>, app: fn() -> Element) -> Self {
        Self::new_internal(app, None)
    }

    /// Create a new embedded editor with shared state.
    ///
    /// This allows the embedded editor to share state with the windowed editor.
    /// The shared state will be available via `use_context::<SharedState>()` in components,
    /// which can then be downcast using `shared_state.get::<T>()`.
    ///
    /// # Arguments
    ///
    /// * `_state` - Editor state (window size, etc.)
    /// * `shared_state` - Shared UI state to inject into Dioxus context
    /// * `app` - The Dioxus component function to render
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn embedded_editor(&mut self) -> Option<Arc<dyn EmbeddedEditor>> {
    ///     Some(Arc::new(DioxusEmbeddedEditor::new_with_state(
    ///         self.params.editor_state.clone(),
    ///         self.ui_state.clone(),
    ///         App,
    ///     )))
    /// }
    /// ```
    pub fn new_with_state<T: Any + Send + Sync + 'static>(
        _state: Arc<DioxusState>,
        shared_state: Arc<T>,
        app: fn() -> Element,
    ) -> Self {
        let wrapped = SharedState::new(shared_state);
        Self::new_internal(app, Some(wrapped))
    }

    /// Internal constructor that handles both with and without shared state.
    fn new_internal(app: fn() -> Element, shared_state: Option<SharedState>) -> Self {
        // Create channels for communication
        // Larger buffer to handle bursts of mouse events
        let (command_tx, command_rx) = bounded::<RenderCommand>(16);
        let (frame_tx, frame_rx) = bounded::<RenderedFrame>(2);

        // Spawn the render thread
        let render_thread = thread::Builder::new()
            .name("dioxus-embed-render".to_string())
            .spawn(move || {
                Self::render_thread_main(command_rx, frame_tx, app, shared_state);
            })
            .expect("Failed to spawn render thread");

        Self {
            command_tx,
            frame_rx,
            cached_frame: Mutex::new(None),
            current_width: AtomicU32::new(0),
            current_height: AtomicU32::new(0),
            needs_render: AtomicBool::new(true),
            render_thread: Mutex::new(Some(render_thread)),
        }
    }

    /// Main function for the render thread.
    ///
    /// This thread owns the DioxusDocument and renders frames on demand.
    fn render_thread_main(
        command_rx: Receiver<RenderCommand>,
        frame_tx: Sender<RenderedFrame>,
        app: fn() -> Element,
        shared_state: Option<SharedState>,
    ) {
        use blitz_traits::events::{
            BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords,
            PointerDetails, UiEvent,
        };
        use dioxus_native::prelude::Modifiers;

        let mut doc: Option<DioxusDocument> = None;
        let mut renderer: Option<VelloCpuImageRenderer> = None;
        let mut last_size: (u32, u32) = (0, 0);
        let mut mouse_buttons = MouseEventButtons::empty();
        let start_time = Instant::now();

        // Clone shared_state for use in document initialization
        let shared_state_for_init = shared_state.clone();

        nice_plug_core::nice_log!("Dioxus embedded render thread started");

        // Track the last scale for re-renders
        let mut last_scale: f32 = 1.0;

        // Poll interval for async tasks (~60fps)
        let poll_interval = std::time::Duration::from_millis(16);

        loop {
            // Use recv_timeout so we can periodically poll the vdom for async updates
            let cmd = match command_rx.recv_timeout(poll_interval) {
                Ok(cmd) => Some(cmd),
                Err(crossbeam::channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam::channel::RecvTimeoutError::Disconnected) => break,
            };

            // Handle command if we got one
            if let Some(cmd) = cmd {
                match cmd {
                    RenderCommand::Render {
                        width,
                        height,
                        scale,
                    } => {
                        if width == 0 || height == 0 {
                            continue;
                        }

                        last_scale = scale;

                        // Initialize document on first render
                        if doc.is_none() {
                            nice_plug_core::nice_log!(
                                "Initializing DioxusDocument for embedded UI ({}x{})",
                                width,
                                height
                            );

                            let vdom = VirtualDom::new(app);
                            let viewport = Viewport::new(width, height, scale, ColorScheme::Dark);
                            let mut d = DioxusDocument::new(
                                vdom,
                                DocumentConfig {
                                    viewport: Some(viewport),
                                    ..Default::default()
                                },
                            );

                            // Inject shared state into Dioxus context if provided
                            if let Some(state) = shared_state_for_init.clone() {
                                d.vdom.in_scope(ScopeId::ROOT, move || {
                                    provide_context(state);
                                });
                            }

                            d.initial_build();
                            doc = Some(d);
                        }

                        // Update or create renderer if size changed
                        if last_size != (width, height) {
                            nice_plug_core::nice_log!(
                                "Resizing embedded renderer to {}x{}",
                                width,
                                height
                            );
                            renderer = Some(VelloCpuImageRenderer::new(width, height));
                            last_size = (width, height);

                            // Update document viewport
                            if let Some(d) = doc.as_mut() {
                                d.inner.borrow_mut().set_viewport(Viewport::new(
                                    width,
                                    height,
                                    scale,
                                    ColorScheme::Dark,
                                ));
                            }
                        }

                        let Some(d) = doc.as_mut() else { continue };
                        let Some(r) = renderer.as_mut() else { continue };

                        // Resolve layout with animation time
                        let animation_time = start_time.elapsed().as_secs_f64();
                        d.inner.borrow_mut().resolve(animation_time);

                        // Render to buffer
                        let mut buffer = vec![0u8; (width * height * 4) as usize];
                        r.render_to_vec(
                            |scene| {
                                blitz_paint::paint_scene(
                                    scene,
                                    &mut *d.inner.borrow_mut(),
                                    scale as f64,
                                    width,
                                    height,
                                    0,
                                    0,
                                );
                            },
                            &mut buffer,
                        );

                        // Send frame (drop if receiver is full - we'll render another)
                        let _ = frame_tx.try_send(RenderedFrame {
                            buffer,
                            width,
                            height,
                        });
                    }
                    RenderCommand::MouseEvent {
                        event_type,
                        x,
                        y,
                        width,
                        height,
                        scale,
                    } => {
                        last_scale = scale;

                        // Ensure document exists
                        if doc.is_none() {
                            let vdom = VirtualDom::new(app);
                            let viewport = Viewport::new(width, height, scale, ColorScheme::Dark);
                            let mut d = DioxusDocument::new(
                                vdom,
                                DocumentConfig {
                                    viewport: Some(viewport),
                                    ..Default::default()
                                },
                            );

                            // Inject shared state into Dioxus context if provided
                            if let Some(state) = shared_state.clone() {
                                d.vdom.in_scope(ScopeId::ROOT, move || {
                                    provide_context(state);
                                });
                            }

                            d.initial_build();
                            doc = Some(d);
                            renderer = Some(VelloCpuImageRenderer::new(width, height));
                            last_size = (width, height);
                        }

                        if let Some(d) = doc.as_mut() {
                            // Create blitz pointer event. Newer blitz unified mouse/touch/pen
                            // into pointer events (`UiEvent::Pointer*` + `BlitzPointerEvent`).
                            let mods = Modifiers::empty();
                            let coords = PointerCoords {
                                page_x: x,
                                page_y: y,
                                screen_x: x,
                                screen_y: y,
                                client_x: x,
                                client_y: y,
                            };
                            let make = |buttons| BlitzPointerEvent {
                                id: BlitzPointerId::Mouse,
                                is_primary: true,
                                coords,
                                button: MouseEventButton::Main,
                                buttons,
                                mods,
                                details: PointerDetails::default(),
                                element: Default::default(),
                            };

                            let ui_event = match event_type {
                                MouseEventType::Move => UiEvent::PointerMove(make(mouse_buttons)),
                                MouseEventType::Down => {
                                    mouse_buttons |=
                                        MouseEventButtons::from(MouseEventButton::Main);
                                    UiEvent::PointerDown(make(mouse_buttons))
                                }
                                MouseEventType::Up => {
                                    mouse_buttons &=
                                        !MouseEventButtons::from(MouseEventButton::Main);
                                    UiEvent::PointerUp(make(mouse_buttons))
                                }
                            };

                            d.handle_ui_event(ui_event);

                            // Poll the virtual DOM to process any state updates from event handlers
                            d.poll(None);

                            // After handling mouse event, re-render immediately
                            if let Some(r) = renderer.as_mut() {
                                let animation_time = start_time.elapsed().as_secs_f64();
                                d.inner.borrow_mut().resolve(animation_time);

                                let (width, height) = last_size;
                                let mut buffer = vec![0u8; (width * height * 4) as usize];
                                r.render_to_vec(
                                    |scene| {
                                        blitz_paint::paint_scene(
                                            scene,
                                            &mut *d.inner.borrow_mut(),
                                            last_scale as f64,
                                            width,
                                            height,
                                            0,
                                            0,
                                        );
                                    },
                                    &mut buffer,
                                );

                                let _ = frame_tx.try_send(RenderedFrame {
                                    buffer,
                                    width,
                                    height,
                                });
                            }
                        }
                    }
                    RenderCommand::Shutdown => {
                        nice_plug_core::nice_log!("Dioxus embedded render thread shutting down");
                        break;
                    }
                }
            }

            // Periodically poll the vdom for async updates (timers, futures, etc.)
            // This runs even when no commands are received
            if let Some(d) = doc.as_mut() {
                // Poll the virtual DOM - returns true if there's more work to do
                let has_work = d.poll(None);

                // Always re-render to keep UI responsive
                // The CPU cost is acceptable at 60fps for embedded UIs
                let (width, height) = last_size;
                if width > 0 && height > 0 {
                    if let Some(r) = renderer.as_mut() {
                        let animation_time = start_time.elapsed().as_secs_f64();
                        d.inner.borrow_mut().resolve(animation_time);

                        let mut buffer = vec![0u8; (width * height * 4) as usize];
                        r.render_to_vec(
                            |scene| {
                                blitz_paint::paint_scene(
                                    scene,
                                    &mut *d.inner.borrow_mut(),
                                    last_scale as f64,
                                    width,
                                    height,
                                    0,
                                    0,
                                );
                            },
                            &mut buffer,
                        );

                        // Only send if channel has space
                        let _ = frame_tx.try_send(RenderedFrame {
                            buffer,
                            width,
                            height,
                        });
                    }
                }

                // If there's pending async work, don't wait - immediately poll again
                if has_work {
                    continue;
                }
            }
        }
    }

    /// Request a render at the given size.
    fn request_render(&self, width: u32, height: u32, scale: f32) {
        let old_width = self.current_width.swap(width, Ordering::Relaxed);
        let old_height = self.current_height.swap(height, Ordering::Relaxed);

        // Only send if dimensions changed or we need a fresh render
        if old_width != width
            || old_height != height
            || self.needs_render.swap(false, Ordering::Relaxed)
        {
            let _ = self.command_tx.try_send(RenderCommand::Render {
                width,
                height,
                scale,
            });
        }
    }

    /// Try to receive new frames and update the cache.
    fn update_cached_frame(&self) {
        // Drain all available frames, keeping only the latest
        while let Ok(frame) = self.frame_rx.try_recv() {
            *self.cached_frame.lock().unwrap() = Some(frame);
        }
    }

    /// Mark that we need a fresh render (e.g., after parameter changes).
    #[allow(dead_code)]
    pub fn invalidate(&self) {
        self.needs_render.store(true, Ordering::Relaxed);
    }
}

impl Drop for DioxusEmbeddedEditor {
    fn drop(&mut self) {
        // Signal the render thread to shut down
        let _ = self.command_tx.send(RenderCommand::Shutdown);

        // Wait for the thread to finish
        if let Some(handle) = self.render_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

// SAFETY: All communication happens via Send+Sync channels.
// The non-Send DioxusDocument lives entirely on the render thread.
// The render thread is properly joined on drop.
unsafe impl Send for DioxusEmbeddedEditor {}
unsafe impl Sync for DioxusEmbeddedEditor {}

impl EmbeddedEditor for DioxusEmbeddedEditor {
    fn is_available(&self) -> bool {
        true
    }

    fn size_hints(&self, context: EmbedContext, _dpi: f32) -> Option<EmbedSizeHints> {
        match context {
            EmbedContext::Mcp => Some(EmbedSizeHints {
                preferred_aspect: 0.5, // MCP: prefer taller for vertical mixer strips
                minimum_aspect: 0.1,
                min_width: 60,
                min_height: 60,
                max_width: 2000,
                max_height: 2000,
            }),
            _ => Some(EmbedSizeHints {
                preferred_aspect: 2.0, // TCP: prefer wider
                minimum_aspect: 0.1,
                min_width: 60,
                min_height: 60,
                max_width: 2000,
                max_height: 2000,
            }),
        }
    }

    fn paint(&self, bitmap: &mut EmbedBitmap<'_>, info: &EmbedDrawInfo) -> bool {
        let width = bitmap.width;
        let height = bitmap.height;

        if width == 0 || height == 0 {
            return false;
        }

        // Request a render (non-blocking)
        self.request_render(width, height, info.dpi);

        // Check for new frames
        self.update_cached_frame();

        // Paint from cached frame
        let cache = self.cached_frame.lock().unwrap();
        if let Some(frame) = cache.as_ref() {
            // If dimensions match exactly, use cached frame directly
            if frame.width == width && frame.height == height {
                copy_rgba_to_bitmap(&frame.buffer, bitmap, width, height);
                return true;
            }

            // If we have a cached frame but dimensions don't match,
            // scale it to avoid flicker while waiting for new render
            scale_rgba_to_bitmap(
                &frame.buffer,
                frame.width,
                frame.height,
                bitmap,
                width,
                height,
            );
            return true;
        }

        // No cached frame yet - paint a placeholder
        bitmap.clear(EmbedBitmap::rgba(26, 26, 30, 255));
        true
    }

    fn mouse_event(&self, event: EmbedMouseEvent, info: &EmbedDrawInfo) -> u32 {
        let event_type = match event {
            EmbedMouseEvent::Move => MouseEventType::Move,
            EmbedMouseEvent::LeftDown => {
                nice_plug_core::nice_log!(
                    "[EMBED] LeftDown at ({}, {})",
                    info.mouse_x,
                    info.mouse_y
                );
                MouseEventType::Down
            }
            EmbedMouseEvent::LeftUp => {
                nice_plug_core::nice_log!("[EMBED] LeftUp at ({}, {})", info.mouse_x, info.mouse_y);
                MouseEventType::Up
            }
            // Ignore other events for now
            _ => return 0,
        };

        let width = self.current_width.load(Ordering::Relaxed);
        let height = self.current_height.load(Ordering::Relaxed);

        if width == 0 || height == 0 {
            nice_plug_core::nice_log!("[EMBED] mouse_event: width/height is 0, ignoring");
            return 0;
        }

        // Send mouse event to render thread (use blocking send for reliability)
        match self.command_tx.try_send(RenderCommand::MouseEvent {
            event_type,
            x: info.mouse_x as f32,
            y: info.mouse_y as f32,
            width,
            height,
            scale: info.dpi,
        }) {
            Ok(_) => {}
            Err(e) => {
                nice_plug_core::nice_log!("[EMBED] Failed to send mouse event: {:?}", e);
            }
        }

        // Request a re-render after mouse events
        self.needs_render.store(true, Ordering::Relaxed);

        // Return INVALIDATE to request redraw
        embed_flags::HANDLED | embed_flags::INVALIDATE
    }
}

/// Apply gamma correction to compensate for color space differences.
/// Vello CPU outputs colors that appear washed out, this darkens them slightly.
/// Using gamma 1.6 as a compromise (less aggressive than 2.2).
#[inline]
fn apply_gamma(value: u8) -> u8 {
    let normalized = value as f32 / 255.0;
    let corrected = normalized.powf(1.6);
    (corrected * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Copy an RGBA buffer to an EmbedBitmap with gamma correction.
///
/// Vello CPU outputs linear RGB which appears washed out on displays.
/// We apply gamma 2.2 to correct the colors.
fn copy_rgba_to_bitmap(buffer: &[u8], bitmap: &mut EmbedBitmap<'_>, width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 3 < buffer.len() {
                let r = apply_gamma(buffer[idx]);
                let g = apply_gamma(buffer[idx + 1]);
                let b = apply_gamma(buffer[idx + 2]);
                let a = buffer[idx + 3]; // Alpha doesn't need gamma
                bitmap.set_pixel(x, y, EmbedBitmap::rgba(r, g, b, a));
            }
        }
    }
}

/// Scale an RGBA buffer to fit a different-sized bitmap using nearest-neighbor interpolation.
///
/// This is used to display a cached frame while waiting for a re-render at the new size,
/// which reduces flicker during resize operations.
fn scale_rgba_to_bitmap(
    src_buffer: &[u8],
    src_width: u32,
    src_height: u32,
    bitmap: &mut EmbedBitmap<'_>,
    dst_width: u32,
    dst_height: u32,
) {
    if src_width == 0 || src_height == 0 || dst_width == 0 || dst_height == 0 {
        return;
    }

    for dst_y in 0..dst_height {
        for dst_x in 0..dst_width {
            // Map destination pixel to source pixel (nearest neighbor)
            let src_x = (dst_x * src_width / dst_width).min(src_width - 1);
            let src_y = (dst_y * src_height / dst_height).min(src_height - 1);

            let idx = ((src_y * src_width + src_x) * 4) as usize;
            if idx + 3 < src_buffer.len() {
                let r = apply_gamma(src_buffer[idx]);
                let g = apply_gamma(src_buffer[idx + 1]);
                let b = apply_gamma(src_buffer[idx + 2]);
                let a = src_buffer[idx + 3];
                bitmap.set_pixel(dst_x, dst_y, EmbedBitmap::rgba(r, g, b, a));
            }
        }
    }
}
