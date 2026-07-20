//! A draggable resize handle for window resizing.

use crate::state::DioxusState;
use dioxus_native::prelude::*;
use std::sync::Arc;

/// A resize handle widget that can be placed in the corner of a window.
///
/// When dragged, it requests a window resize through the DioxusState.
///
/// # Example
///
/// ```ignore
/// use nice_plug_dioxus::widgets::ResizeHandle;
///
/// fn App() -> Element {
///     rsx! {
///         div {
///             style: "width: 100%; height: 100%; position: relative;",
///             
///             // Your content here...
///             
///             // Place resize handle in bottom-right corner
///             ResizeHandle {
///                 min_width: 400,
///                 min_height: 300,
///             }
///         }
///     }
/// }
/// ```
#[component]
pub fn ResizeHandle(
    /// Minimum window width in logical pixels.
    #[props(default = 200)]
    min_width: u32,
    /// Minimum window height in logical pixels.
    #[props(default = 150)]
    min_height: u32,
    /// Maximum window width in logical pixels (0 = no limit).
    #[props(default = 0)]
    max_width: u32,
    /// Maximum window height in logical pixels (0 = no limit).
    #[props(default = 0)]
    max_height: u32,
    /// Size of the resize handle in pixels.
    #[props(default = 16)]
    size: u32,
    /// Color of the resize handle lines.
    #[props(default = "rgba(128, 128, 128, 0.6)")]
    color: &'static str,
) -> Element {
    // Get the DioxusState from context - may not be available in embedded editor
    let dioxus_state = try_use_context::<Arc<DioxusState>>();

    // If no DioxusState context, don't render anything (embedded editor case)
    let Some(dioxus_state) = dioxus_state else {
        return rsx! {};
    };

    // Track drag state using signals
    let mut is_dragging = use_signal(|| false);
    // Track the last cursor position for incremental delta calculation
    let mut last_cursor_pos = use_signal(|| (0.0f64, 0.0f64));
    // Track accumulated size during drag
    let mut accumulated_size = use_signal(|| (0u32, 0u32));
    // Track if we've initialized the cursor position
    let mut cursor_initialized = use_signal(|| false);

    let handle_style = format!(
        r#"
        position: absolute;
        bottom: 0;
        right: 0;
        width: {size}px;
        height: {size}px;
        cursor: nwse-resize;
        user-select: none;
        background: transparent;
        "#
    );

    // SVG resize grip icon - diagonal lines pattern
    let grip_svg = format!(
        r#"
        <svg width="{size}" height="{size}" viewBox="0 0 32 32" fill="none" xmlns="http://www.w3.org/2000/svg">
            <line x1="28" y1="32" x2="32" y2="28" stroke="{color}" stroke-width="2" stroke-linecap="round"/>
            <line x1="20" y1="32" x2="32" y2="20" stroke="{color}" stroke-width="2" stroke-linecap="round"/>
            <line x1="12" y1="32" x2="32" y2="12" stroke="{color}" stroke-width="2" stroke-linecap="round"/>
        </svg>
        "#
    );

    // Clone state for closures
    let state_for_mousedown = dioxus_state.clone();
    let state_for_mousemove = dioxus_state.clone();
    let state_for_overlay = dioxus_state.clone();

    rsx! {
        div {
            style: "{handle_style}",
            dangerous_inner_html: "{grip_svg}",

            onmousedown: {
                let state = state_for_mousedown.clone();
                move |evt| {
                    is_dragging.set(true);
                    cursor_initialized.set(false);
                    // Initialize accumulated size to current window size
                    let (w, h) = state.size();
                    nice_plug_core::nice_log!("[RESIZE HANDLE] mousedown - starting size: {}x{}", w, h);
                    accumulated_size.set((w, h));
                    evt.stop_propagation();
                }
            },

            onmousemove: {
                let state = state_for_mousemove.clone();
                move |evt| {
                    if *is_dragging.read() {
                        let coords = evt.client_coordinates();
                        let current_pos = (coords.x, coords.y);

                        // On first move after mousedown, just initialize position
                        if !*cursor_initialized.read() {
                            last_cursor_pos.set(current_pos);
                            cursor_initialized.set(true);
                            return;
                        }

                        // Calculate incremental delta from last position
                        let (last_x, last_y) = *last_cursor_pos.read();
                        let dx = current_pos.0 - last_x;
                        let dy = current_pos.1 - last_y;

                        // Update last position for next frame
                        last_cursor_pos.set(current_pos);

                        // Accumulate into size
                        let (acc_w, acc_h) = *accumulated_size.read();
                        let mut new_width = (acc_w as f64 + dx).round().max(0.0) as u32;
                        let mut new_height = (acc_h as f64 + dy).round().max(0.0) as u32;

                        // Apply constraints
                        new_width = new_width.max(min_width);
                        new_height = new_height.max(min_height);
                        if max_width > 0 {
                            new_width = new_width.min(max_width);
                        }
                        if max_height > 0 {
                            new_height = new_height.min(max_height);
                        }

                        // Update accumulated size
                        accumulated_size.set((new_width, new_height));

                        // Request resize
                        nice_plug_core::nice_log!("[RESIZE HANDLE] requesting: {}x{} (delta: {:.1}, {:.1})", new_width, new_height, dx, dy);
                        state.request_resize(new_width, new_height);
                    }
                }
            },

            onmouseup: move |_evt| {
                nice_plug_core::nice_log!("[RESIZE HANDLE] mouseup - drag ended");
                is_dragging.set(false);
            },

            onmouseleave: move |_evt| {
                // Don't stop dragging - overlay handles it
            },
        }

        // Global mouse event catcher when dragging (invisible overlay)
        if *is_dragging.read() {
            div {
                style: "position: fixed; top: 0; left: 0; right: 0; bottom: 0; cursor: nwse-resize; z-index: 9999;",

                onmousemove: {
                    let state = state_for_overlay.clone();
                    move |evt| {
                        let coords = evt.client_coordinates();
                        let current_pos = (coords.x, coords.y);

                        // On first move, initialize
                        if !*cursor_initialized.read() {
                            last_cursor_pos.set(current_pos);
                            cursor_initialized.set(true);
                            return;
                        }

                        // Calculate incremental delta
                        let (last_x, last_y) = *last_cursor_pos.read();
                        let dx = current_pos.0 - last_x;
                        let dy = current_pos.1 - last_y;

                        // Update last position
                        last_cursor_pos.set(current_pos);

                        // Accumulate into size
                        let (acc_w, acc_h) = *accumulated_size.read();
                        let mut new_width = (acc_w as f64 + dx).round().max(0.0) as u32;
                        let mut new_height = (acc_h as f64 + dy).round().max(0.0) as u32;

                        // Apply constraints
                        new_width = new_width.max(min_width);
                        new_height = new_height.max(min_height);
                        if max_width > 0 {
                            new_width = new_width.min(max_width);
                        }
                        if max_height > 0 {
                            new_height = new_height.min(max_height);
                        }

                        // Update accumulated size
                        accumulated_size.set((new_width, new_height));

                        // Request resize
                        state.request_resize(new_width, new_height);
                    }
                },

                onmouseup: move |_evt| {
                    is_dragging.set(false);
                },
            }
        }
    }
}
