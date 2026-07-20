//! REAPER embedded UI CLAP extension support.
//!
//! This module provides support for REAPER's `cockos.reaper_embedui` CLAP extension,
//! which allows plugins to render an inline UI directly in REAPER's track/mixer panel.

// This module is a faithful FFI shim around REAPER's C ABI: every function here is
// `unsafe fn` whose entire body is an unsafe operation against host-provided raw
// pointers. The safety contract is documented per-function, so we opt out of edition
// 2024's `unsafe_op_in_unsafe_fn` rather than wrapping each line. `dead_code` covers
// the complete-but-not-all-exercised LICE/draw-info surface kept for fidelity.
#![allow(unsafe_op_in_unsafe_fn, dead_code)]

use std::ffi::{c_void, CStr};
use std::slice;
use std::sync::Arc;

use clap_sys::plugin::clap_plugin;

use crate::prelude::{
    EmbedBitmap, EmbedContext, EmbedDrawInfo, EmbedMouseEvent, EmbedSizeHints, EmbeddedEditor,
};

/// The extension ID for REAPER's embedded UI CLAP extension.
pub const CLAP_EXT_REAPER_EMBED_UI: &CStr = c"cockos.reaper_embedui";

/// REAPER FX embed message constants (from reaper_plugin_fx_embed.h)
pub mod embed_msg {
    /// Return 1 if embedding is supported and available, -1 if supported but unavailable, 0 if not supported.
    pub const IS_SUPPORTED: isize = 0x0000;
    /// Called when embedding begins (return value ignored).
    pub const CREATE: isize = 0x0001;
    /// Called when embedding ends (return value ignored).
    pub const DESTROY: isize = 0x0002;
    /// Draw embedded UI. parm1 = bitmap ptr, parm2 = draw info ptr.
    pub const PAINT: isize = 0x000F;
    /// Set mouse cursor. parm2 = draw info ptr.
    pub const SETCURSOR: isize = 0x0020;
    /// Get size hints. parm2 = size hints ptr.
    pub const GETMINMAXINFO: isize = 0x0024;
    /// Mouse move. parm2 = draw info ptr.
    pub const MOUSEMOVE: isize = 0x0200;
    /// Left button down. parm2 = draw info ptr.
    pub const LBUTTONDOWN: isize = 0x0201;
    /// Left button up. parm2 = draw info ptr.
    pub const LBUTTONUP: isize = 0x0202;
    /// Left button double-click. parm2 = draw info ptr.
    pub const LBUTTONDBLCLK: isize = 0x0203;
    /// Right button down. parm2 = draw info ptr.
    pub const RBUTTONDOWN: isize = 0x0204;
    /// Right button up. parm2 = draw info ptr.
    pub const RBUTTONUP: isize = 0x0205;
    /// Right button double-click. parm2 = draw info ptr.
    pub const RBUTTONDBLCLK: isize = 0x0206;
    /// Mouse wheel. parm2 = draw info ptr (mousewheel_amt field has amount).
    pub const MOUSEWHEEL: isize = 0x020A;
}

/// REAPER FX embed draw info flags
pub mod draw_info_flags {
    pub const PAINT_OPTIONAL: i32 = 1;
    pub const IS_RETINA: i32 = 0x00100;
    pub const LBUTTON_CAPTURED: i32 = 0x10000;
    pub const RBUTTON_CAPTURED: i32 = 0x20000;
}

/// Raw REAPER FX embed draw info structure (matches REAPER_FXEMBED_DrawInfo)
#[repr(C)]
pub struct RawEmbedDrawInfo {
    pub context: i32,
    pub dpi: i32, // 24.8 fixed point (256 = 100%)
    pub mousewheel_amt: i32,
    pub _res2: f64,
    pub width: i32,
    pub height: i32,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub flags: i32,
    pub _res3: i32,
    pub spare: [*mut c_void; 6],
}

/// Raw REAPER FX embed size hints structure (matches REAPER_FXEMBED_SizeHints)
#[repr(C)]
pub struct RawEmbedSizeHints {
    pub preferred_aspect: i32, // 16.16 fixed point
    pub minimum_aspect: i32,   // 16.16 fixed point
    pub flags: i32,            // (flags&15) is context
    pub dpi: i32,              // 256 = 100%
    pub _res3: i32,
    pub _res4: i32,
    pub min_width: i32,
    pub min_height: i32,
    pub max_width: i32,
    pub max_height: i32,
}

/// The vtable for REAPER's embedded UI CLAP extension.
///
/// From reaper_plugin_fx_embed.h:
/// ```c
/// struct clap_plugin_reaper_embedui {
///     INT_PTR (CLAP_ABI *inline_editor)(const clap_plugin_t *plugin, int msg, void *param1, void *param2);
/// };
/// ```
#[repr(C)]
pub struct ClapPluginReaperEmbedUi {
    pub inline_editor: Option<
        unsafe extern "C" fn(
            plugin: *const clap_plugin,
            msg: i32,
            param1: *mut c_void,
            param2: *mut c_void,
        ) -> isize,
    >,
}

impl RawEmbedDrawInfo {
    /// Convert to the high-level EmbedDrawInfo.
    pub fn to_draw_info(&self) -> EmbedDrawInfo {
        // DPI is 24.8 fixed point, so 256 = 1.0
        let dpi = if self.dpi > 0 {
            self.dpi as f32 / 256.0
        } else {
            1.0
        };

        EmbedDrawInfo {
            context: EmbedContext::from(self.context),
            dpi,
            width: self.width,
            height: self.height,
            mouse_x: self.mouse_x,
            mouse_y: self.mouse_y,
            flags: self.flags as u32,
            mousewheel_amt: self.mousewheel_amt,
        }
    }
}

impl RawEmbedSizeHints {
    /// Fill from high-level EmbedSizeHints.
    #[allow(clippy::wrong_self_convention)] // fills self in place, CLAP style
    pub fn from_size_hints(&mut self, hints: &EmbedSizeHints) {
        // Convert aspect ratio to 16.16 fixed point (65536 = 1:1)
        self.preferred_aspect = if hints.preferred_aspect > 0.0 {
            (hints.preferred_aspect * 65536.0) as i32
        } else {
            0
        };
        self.minimum_aspect = if hints.minimum_aspect > 0.0 {
            (hints.minimum_aspect * 65536.0) as i32
        } else {
            0
        };
        self.min_width = hints.min_width;
        self.min_height = hints.min_height;
        self.max_width = hints.max_width;
        self.max_height = hints.max_height;
    }
}

/// Trait to access the raw bitmap interface from REAPER.
///
/// This is a C++ class alias of LICE_IBitmap. We only need to call the virtual methods,
/// so we'll use a vtable-based approach.
///
/// The Itanium C++ ABI (used on macOS/Linux) places the destructor at specific offsets.
/// We need to account for both complete object and deleting destructors.
///
/// LICE_IBitmap vtable layout (Itanium ABI on macOS):
/// [0] offset to top (usually 0)  - NOT a function pointer
/// [1] RTTI pointer               - NOT a function pointer  
/// [2] destructor (complete)
/// [3] destructor (deleting)
/// [4] getBits
/// [5] getWidth
/// [6] getHeight
/// [7] getRowSpan
/// [8] isFlipped
/// [9] resize
/// [10] getDC
/// [11] Extended
///
/// But the vtable pointer in the object points AFTER the offset-to-top and RTTI,
/// so from our perspective:
/// [0] destructor (complete)
/// [1] destructor (deleting)
/// [2] getBits
/// ... etc
#[repr(C)]
struct IBitmapVtable {
    // Itanium ABI: two destructor slots for classes with virtual destructors
    _destructor_complete: *const c_void,
    _destructor_deleting: *const c_void,

    // virtual unsigned int *getBits()=0;
    get_bits: Option<unsafe extern "C" fn(*mut c_void) -> *mut u32>,
    // virtual int getWidth()=0;
    get_width: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    // virtual int getHeight()=0;
    get_height: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    // virtual int getRowSpan()=0; // in u32 units
    get_row_span: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    // virtual bool isFlipped() { return false; }
    is_flipped: Option<unsafe extern "C" fn(*mut c_void) -> bool>,
    // virtual bool resize(int w, int h)=0;
    _resize: *const c_void,
    // virtual void *getDC() { return 0; }
    _get_dc: *const c_void,
    // virtual INT_PTR Extended(int id, void* data) { return 0; }
    _extended: *const c_void,
}

/// Raw bitmap pointer from REAPER (alias of LICE_IBitmap)
#[repr(C)]
struct RawBitmap {
    vtable: *const IBitmapVtable,
}

impl RawBitmap {
    unsafe fn get_bits(&self) -> *mut u32 {
        let vtable = &*self.vtable;
        match vtable.get_bits {
            Some(f) => f(self as *const _ as *mut c_void),
            None => std::ptr::null_mut(),
        }
    }

    unsafe fn get_width(&self) -> i32 {
        let vtable = &*self.vtable;
        match vtable.get_width {
            Some(f) => f(self as *const _ as *mut c_void),
            None => 0,
        }
    }

    unsafe fn get_height(&self) -> i32 {
        let vtable = &*self.vtable;
        match vtable.get_height {
            Some(f) => f(self as *const _ as *mut c_void),
            None => 0,
        }
    }

    unsafe fn get_row_span(&self) -> i32 {
        let vtable = &*self.vtable;
        match vtable.get_row_span {
            Some(f) => f(self as *const _ as *mut c_void),
            None => 0,
        }
    }

    unsafe fn is_flipped(&self) -> bool {
        let vtable = &*self.vtable;
        match vtable.is_flipped {
            Some(f) => f(self as *const _ as *mut c_void),
            None => false,
        }
    }
}

/// Handle the REAPER embedded UI extension callback.
///
/// # Safety
///
/// This function is called from C code with raw pointers. The caller must ensure:
/// - `embedded_editor` is a valid Arc
/// - `param1` and `param2` are valid pointers for the given message type
pub unsafe fn handle_embed_message(
    embedded_editor: &Arc<dyn EmbeddedEditor>,
    msg: i32,
    param1: *mut c_void,
    param2: *mut c_void,
) -> isize {
    let msg = msg as isize;

    match msg {
        embed_msg::IS_SUPPORTED => {
            if embedded_editor.is_available() {
                1 // Available
            } else {
                0 // Not supported
            }
        }

        embed_msg::CREATE => {
            embedded_editor.create();
            0
        }

        embed_msg::DESTROY => {
            embedded_editor.destroy();
            0
        }

        embed_msg::PAINT => {
            // param1 = REAPER_FXEMBED_IBitmap*, param2 = REAPER_FXEMBED_DrawInfo*
            if param1.is_null() || param2.is_null() {
                return 0;
            }

            let raw_bitmap = &*(param1 as *const RawBitmap);
            let raw_info = &*(param2 as *const RawEmbedDrawInfo);

            // Validate vtable pointer before dereferencing
            if raw_bitmap.vtable.is_null() {
                return 0;
            }

            // Get draw info which contains the authoritative dimensions
            let info = raw_info.to_draw_info();

            // Use DrawInfo dimensions - these are reliable
            let width = info.width;
            let height = info.height;

            // Get bits pointer from bitmap (we still need to call the vtable for this)
            let bits_ptr = raw_bitmap.get_bits();

            // Calculate row_span from width (LICE bitmaps are typically tightly packed)
            // Or we can try to get it from the bitmap, but use width as fallback
            let row_span_from_bitmap = raw_bitmap.get_row_span();
            // Row span should be at least width, use width if the vtable call returned garbage
            let row_span = if row_span_from_bitmap >= width {
                row_span_from_bitmap
            } else {
                width // Assume tightly packed if vtable is unreliable
            };

            // isFlipped is less critical, default to false if vtable is unreliable
            let flipped = false; // Safe default

            if bits_ptr.is_null() || width <= 0 || height <= 0 || row_span <= 0 {
                return 0;
            }

            let buffer_len = (row_span * height) as usize;
            let bits = slice::from_raw_parts_mut(bits_ptr, buffer_len);

            let mut bitmap =
                EmbedBitmap::new(bits, width as u32, height as u32, row_span as u32, flipped);

            if embedded_editor.paint(&mut bitmap, &info) {
                1 // Drawing occurred
            } else {
                0 // No drawing (optional paint was skipped)
            }
        }

        embed_msg::SETCURSOR => {
            if param2.is_null() {
                return 0;
            }
            let raw_info = &*(param2 as *const RawEmbedDrawInfo);
            let info = raw_info.to_draw_info();

            if embedded_editor.set_cursor(&info) {
                1 // Cursor was set
            } else {
                0
            }
        }

        embed_msg::GETMINMAXINFO => {
            if param2.is_null() {
                return 0;
            }
            let raw_hints = &mut *(param2 as *mut RawEmbedSizeHints);
            let context = EmbedContext::from(raw_hints.flags & 0xf);
            let dpi = if raw_hints.dpi > 0 {
                raw_hints.dpi as f32 / 256.0
            } else {
                1.0
            };

            match embedded_editor.size_hints(context, dpi) {
                Some(hints) => {
                    raw_hints.from_size_hints(&hints);
                    1 // Hints provided
                }
                None => 0,
            }
        }

        embed_msg::MOUSEMOVE
        | embed_msg::LBUTTONDOWN
        | embed_msg::LBUTTONUP
        | embed_msg::LBUTTONDBLCLK
        | embed_msg::RBUTTONDOWN
        | embed_msg::RBUTTONUP
        | embed_msg::RBUTTONDBLCLK
        | embed_msg::MOUSEWHEEL => {
            if param2.is_null() {
                return 0;
            }
            let raw_info = &*(param2 as *const RawEmbedDrawInfo);
            let info = raw_info.to_draw_info();

            let event = match msg {
                embed_msg::MOUSEMOVE => EmbedMouseEvent::Move,
                embed_msg::LBUTTONDOWN => EmbedMouseEvent::LeftDown,
                embed_msg::LBUTTONUP => EmbedMouseEvent::LeftUp,
                embed_msg::LBUTTONDBLCLK => EmbedMouseEvent::LeftDoubleClick,
                embed_msg::RBUTTONDOWN => EmbedMouseEvent::RightDown,
                embed_msg::RBUTTONUP => EmbedMouseEvent::RightUp,
                embed_msg::RBUTTONDBLCLK => EmbedMouseEvent::RightDoubleClick,
                embed_msg::MOUSEWHEEL => EmbedMouseEvent::Wheel {
                    amount: raw_info.mousewheel_amt,
                },
                _ => return 0,
            };

            embedded_editor.mouse_event(event, &info) as isize
        }

        // NOBORDER - return 0 to use default border, return 1 to request no border
        0x100001 => 0,

        // HITTEST - return 1 if click should pass through
        0x100002 => 0,

        _ => 0,
    }
}
