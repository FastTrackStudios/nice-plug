//! Asset loading and font registration.

/// The default theme CSS for nice_plug_dioxus widgets.
pub const DEFAULT_THEME: &str = include_str!("../assets/theme.css");

/// Register the default theme stylesheet with a Dioxus document.
///
/// This is automatically called by `create_dioxus_editor`, but can be called
/// manually if you need to register it at a different point.
pub fn register_default_theme() {
    // The theme is loaded via the Dioxus `document::Stylesheet` component
    // in the widgets, so this function is mostly a placeholder for future
    // font registration and other asset loading.
}
