//! Types and traits for REAPER's embedded/inline FX UI.
//!
//! This allows plugins to render a small inline UI directly in REAPER's track control panel (TCP)
//! or mixer control panel (MCP), without creating a separate window.
//!
//! # Usage
//!
//! Implement the [`EmbeddedEditor`] trait and return it from [`Plugin::embedded_editor()`].
//!
//! ```ignore
//! fn embedded_editor(&mut self) -> Option<Arc<dyn EmbeddedEditor>> {
//!     Some(Arc::new(MyEmbeddedEditor::new(self.params.clone())))
//! }
//! ```

/// The context in which the embedded UI is being displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedContext {
    /// Unknown context (REAPER v6.23 and earlier).
    Unknown,
    /// Track Control Panel.
    Tcp,
    /// Mixer Control Panel.
    Mcp,
}

impl From<i32> for EmbedContext {
    fn from(value: i32) -> Self {
        match value {
            1 => EmbedContext::Tcp,
            2 => EmbedContext::Mcp,
            _ => EmbedContext::Unknown,
        }
    }
}

/// Mouse event types for embedded UI interaction.
#[derive(Debug, Clone, Copy)]
pub enum EmbedMouseEvent {
    /// Mouse moved.
    Move,
    /// Left button pressed.
    LeftDown,
    /// Left button released.
    LeftUp,
    /// Left button double-clicked.
    LeftDoubleClick,
    /// Right button pressed.
    RightDown,
    /// Right button released.
    RightUp,
    /// Right button double-clicked.
    RightDoubleClick,
    /// Mouse wheel scrolled. The amount is typically 120 per "step".
    Wheel { amount: i32 },
}

/// Flags that can be returned from mouse event handlers.
pub mod embed_flags {
    /// Indicates that the handler processed the event (e.g., set a cursor).
    pub const HANDLED: u32 = 0x0000001;
    /// Request an immediate (non-optional) redraw after this event.
    pub const INVALIDATE: u32 = 0x1000000;
}

/// Flags that may be set in [`EmbedDrawInfo::flags`].
pub mod draw_info_flags {
    /// If set, the paint is optional - return 0 from paint() if nothing changed.
    pub const PAINT_OPTIONAL: u32 = 1;
    /// Retina/HiDPI display - width/height and mouse coordinates are doubled.
    pub const IS_RETINA: u32 = 0x00100;
    /// Left mouse button is currently captured.
    pub const LBUTTON_CAPTURED: u32 = 0x10000;
    /// Right mouse button is currently captured.
    pub const RBUTTON_CAPTURED: u32 = 0x20000;
}

/// Information provided during paint and mouse events.
#[derive(Debug, Clone)]
pub struct EmbedDrawInfo {
    /// The context (TCP, MCP, or Unknown).
    pub context: EmbedContext,
    /// DPI scaling factor (1.0 = 100%). Derived from 24.8 fixed point.
    pub dpi: f32,
    /// Width of the drawing area in pixels.
    pub width: i32,
    /// Height of the drawing area in pixels.
    pub height: i32,
    /// Mouse X coordinate relative to the drawing area.
    pub mouse_x: i32,
    /// Mouse Y coordinate relative to the drawing area.
    pub mouse_y: i32,
    /// Various flags, see [`draw_info_flags`].
    pub flags: u32,
    /// Mouse wheel amount (for wheel events). Typically 120 per step.
    pub mousewheel_amt: i32,
}

impl EmbedDrawInfo {
    /// Returns true if this is an optional paint (no change since last draw is acceptable).
    pub fn is_paint_optional(&self) -> bool {
        self.flags & draw_info_flags::PAINT_OPTIONAL != 0
    }

    /// Returns true if the left mouse button is currently captured.
    pub fn is_left_button_captured(&self) -> bool {
        self.flags & draw_info_flags::LBUTTON_CAPTURED != 0
    }

    /// Returns true if the right mouse button is currently captured.
    pub fn is_right_button_captured(&self) -> bool {
        self.flags & draw_info_flags::RBUTTON_CAPTURED != 0
    }

    /// Returns true if this is a Retina/HiDPI display where coordinates are doubled.
    pub fn is_retina(&self) -> bool {
        self.flags & draw_info_flags::IS_RETINA != 0
    }
}

/// Size hints for the embedded UI.
#[derive(Debug, Clone)]
pub struct EmbedSizeHints {
    /// Preferred aspect ratio as width/height (1.0 = square, 2.0 = twice as wide as tall).
    /// Use 0.0 for no preference.
    pub preferred_aspect: f32,
    /// Minimum aspect ratio. Use 0.0 for no preference.
    pub minimum_aspect: f32,
    /// Minimum width in pixels, or 0 for no minimum.
    pub min_width: i32,
    /// Minimum height in pixels, or 0 for no minimum.
    pub min_height: i32,
    /// Maximum width in pixels, or 0 for no maximum.
    pub max_width: i32,
    /// Maximum height in pixels, or 0 for no maximum.
    pub max_height: i32,
}

impl Default for EmbedSizeHints {
    fn default() -> Self {
        Self {
            preferred_aspect: 0.0,
            minimum_aspect: 0.0,
            min_width: 0,
            min_height: 0,
            max_width: 0,
            max_height: 0,
        }
    }
}

/// A wrapper around the host-provided bitmap buffer for rendering.
///
/// The bitmap uses BGRA pixel format (blue in the lowest byte, alpha in the highest).
/// Use the `pixel()` and `set_pixel()` methods, or access the raw buffer directly.
pub struct EmbedBitmap<'a> {
    /// Raw pixel data in BGRA format.
    pub bits: &'a mut [u32],
    /// Width of the bitmap in pixels.
    pub width: u32,
    /// Height of the bitmap in pixels.
    pub height: u32,
    /// Row stride in u32 units (may be larger than width for alignment).
    pub row_span: u32,
    /// Whether the bitmap is flipped vertically.
    pub flipped: bool,
}

impl<'a> EmbedBitmap<'a> {
    /// Create a new EmbedBitmap wrapper.
    ///
    /// # Safety
    ///
    /// The `bits` slice must be valid for the entire duration of the paint callback and must
    /// have at least `row_span * height` elements.
    pub unsafe fn new(
        bits: &'a mut [u32],
        width: u32,
        height: u32,
        row_span: u32,
        flipped: bool,
    ) -> Self {
        Self {
            bits,
            width,
            height,
            row_span,
            flipped,
        }
    }

    /// Create a BGRA color value from components.
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
        (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24)
    }

    /// Extract the red component from a pixel.
    #[inline]
    pub fn get_r(pixel: u32) -> u8 {
        ((pixel >> 16) & 0xff) as u8
    }

    /// Extract the green component from a pixel.
    #[inline]
    pub fn get_g(pixel: u32) -> u8 {
        ((pixel >> 8) & 0xff) as u8
    }

    /// Extract the blue component from a pixel.
    #[inline]
    pub fn get_b(pixel: u32) -> u8 {
        (pixel & 0xff) as u8
    }

    /// Extract the alpha component from a pixel.
    #[inline]
    pub fn get_a(pixel: u32) -> u8 {
        ((pixel >> 24) & 0xff) as u8
    }

    /// Get the index into the bits array for a given pixel coordinate.
    #[inline]
    fn pixel_index(&self, x: u32, y: u32) -> usize {
        let y = if self.flipped {
            self.height.saturating_sub(1).saturating_sub(y)
        } else {
            y
        };
        (y * self.row_span + x) as usize
    }

    /// Get the pixel value at (x, y), or None if out of bounds.
    pub fn pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x < self.width && y < self.height {
            let idx = self.pixel_index(x, y);
            self.bits.get(idx).copied()
        } else {
            None
        }
    }

    /// Set the pixel value at (x, y). Does nothing if out of bounds.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x < self.width && y < self.height {
            let idx = self.pixel_index(x, y);
            if let Some(p) = self.bits.get_mut(idx) {
                *p = color;
            }
        }
    }

    /// Fill the entire bitmap with a solid color.
    pub fn clear(&mut self, color: u32) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.set_pixel(x, y, color);
            }
        }
    }

    /// Fill a rectangle with a solid color.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + w).max(0).min(self.width as i32) as u32;
        let y1 = (y + h).max(0).min(self.height as i32) as u32;

        for py in y0..y1 {
            for px in x0..x1 {
                self.set_pixel(px, py, color);
            }
        }
    }

    /// Draw a horizontal line.
    pub fn hline(&mut self, x: i32, y: i32, w: i32, color: u32) {
        if y < 0 || y >= self.height as i32 {
            return;
        }
        let x0 = x.max(0) as u32;
        let x1 = (x + w).max(0).min(self.width as i32) as u32;
        let y = y as u32;
        for px in x0..x1 {
            self.set_pixel(px, y, color);
        }
    }

    /// Draw a vertical line.
    pub fn vline(&mut self, x: i32, y: i32, h: i32, color: u32) {
        if x < 0 || x >= self.width as i32 {
            return;
        }
        let y0 = y.max(0) as u32;
        let y1 = (y + h).max(0).min(self.height as i32) as u32;
        let x = x as u32;
        for py in y0..y1 {
            self.set_pixel(x, py, color);
        }
    }

    /// Draw a rectangle outline (1 pixel border).
    pub fn draw_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.hline(x, y, w, color);
        self.hline(x, y + h - 1, w, color);
        self.vline(x, y, h, color);
        self.vline(x + w - 1, y, h, color);
    }
}

/// An embedded (inline) editor for REAPER's TCP/MCP panel.
///
/// Unlike the regular [`Editor`] trait which creates a windowed UI, this trait renders
/// directly to a bitmap buffer provided by REAPER. This is suitable for simple visualizations
/// and controls that fit in the track/mixer panel.
///
/// # Thread Safety
///
/// All methods may be called from any thread (the GUI thread in practice), so the implementation
/// must be `Send + Sync`.
pub trait EmbeddedEditor: Send + Sync {
    /// Returns whether the embedded UI is currently available.
    ///
    /// Return `true` if the embedded UI can be shown, `-1` equivalent (return `false` and
    /// handle unavailability gracefully) if it's supported but temporarily unavailable.
    ///
    /// The default implementation returns `true`.
    fn is_available(&self) -> bool {
        true
    }

    /// Called when embedding begins.
    ///
    /// Use this to initialize any per-instance state needed for rendering.
    fn create(&self) {}

    /// Called when embedding ends.
    ///
    /// Use this to clean up any per-instance state.
    fn destroy(&self) {}

    /// Report size hints to the host.
    ///
    /// The host uses these hints to determine the initial and preferred size of the
    /// embedded UI area. Return `None` to not provide hints.
    fn size_hints(&self, context: EmbedContext, dpi: f32) -> Option<EmbedSizeHints> {
        let _ = (context, dpi);
        None
    }

    /// Render the embedded UI to the provided bitmap buffer.
    ///
    /// This is called on the GUI thread when the embedded UI needs to be redrawn.
    /// Check [`EmbedDrawInfo::is_paint_optional()`] - if it returns `true` and nothing
    /// has changed since the last draw, you may return `false` to skip rendering.
    ///
    /// Returns `true` if rendering occurred, `false` if skipped.
    fn paint(&self, bitmap: &mut EmbedBitmap<'_>, info: &EmbedDrawInfo) -> bool;

    /// Handle a mouse event.
    ///
    /// Return a combination of [`embed_flags`] to indicate how the event was handled:
    /// - `embed_flags::HANDLED` - The event was processed (e.g., cursor was set)
    /// - `embed_flags::INVALIDATE` - Request an immediate redraw
    ///
    /// The default implementation returns 0 (not handled).
    fn mouse_event(&self, event: EmbedMouseEvent, info: &EmbedDrawInfo) -> u32 {
        let _ = (event, info);
        0
    }

    /// Set the cursor for the current mouse position.
    ///
    /// Return `true` and set the appropriate cursor if you want to override the default,
    /// or `false` to use the default cursor.
    ///
    /// The default implementation returns `false`.
    fn set_cursor(&self, info: &EmbedDrawInfo) -> bool {
        let _ = info;
        false
    }
}
