//! A dropdown/select component for Dioxus.

use dioxus_native::prelude::*;

/// A dropdown/select component.
///
/// # Example
///
/// ```ignore
/// let selected = use_signal(|| 0usize);
/// let options = vec!["Option 1", "Option 2", "Option 3"];
///
/// rsx! {
///     Dropdown {
///         options: options,
///         selected: selected(),
///         on_change: move |idx| selected.set(idx),
///     }
/// }
/// ```
#[component]
pub fn Dropdown<T: ToString + PartialEq + Clone + 'static>(
    /// The list of options to display.
    options: Vec<T>,
    /// The currently selected index.
    selected: usize,
    /// Callback when selection changes.
    on_change: EventHandler<usize>,
    /// Optional label to display above the dropdown.
    #[props(default = None)]
    label: Option<&'static str>,
    /// Width of the dropdown (CSS value).
    #[props(default = "150px")]
    width: &'static str,
) -> Element {
    let mut is_open = use_signal(|| false);

    let selected_text = options
        .get(selected)
        .map(|o| o.to_string())
        .unwrap_or_default();

    let container_style = format!(
        r#"
        position: relative;
        width: {width};
        font-family: system-ui, -apple-system, sans-serif;
        font-size: 13px;
        z-index: 100;
        "#
    );

    let button_style = r#"
        width: 100%;
        padding: 8px 12px;
        background: #2a2a2a;
        border: 1px solid #444;
        border-radius: 4px;
        color: #eee;
        cursor: pointer;
        display: flex;
        justify-content: space-between;
        align-items: center;
        box-sizing: border-box;
    "#;

    let dropdown_style = r#"
        position: absolute;
        top: 100%;
        left: 0;
        right: 0;
        margin-top: 4px;
        background: #2a2a2a;
        border: 1px solid #444;
        border-radius: 4px;
        box-shadow: 0 4px 12px rgba(0, 0, 0, 0.5);
        z-index: 10000;
        max-height: 200px;
        overflow-y: auto;
    "#;

    let option_style = r#"
        padding: 8px 12px;
        cursor: pointer;
        color: #eee;
    "#;

    let _option_hover_style = r#"
        padding: 8px 12px;
        cursor: pointer;
        color: #eee;
        background: #3a3a3a;
    "#;

    let option_selected_style = r#"
        padding: 8px 12px;
        cursor: pointer;
        color: #fff;
        background: #0066cc;
    "#;

    let label_style = r#"
        color: #aaa;
        font-size: 11px;
        margin-bottom: 4px;
        text-transform: uppercase;
        letter-spacing: 0.5px;
    "#;

    // Arrow SVG - use rgb() instead of hex to avoid parsing issues
    let arrow_svg = if *is_open.read() {
        r##"<svg width="12" height="12" viewBox="0 0 12 12" fill="none"><path d="M2 8L6 4L10 8" stroke="rgb(136,136,136)" stroke-width="1.5" stroke-linecap="round"/></svg>"##
    } else {
        r##"<svg width="12" height="12" viewBox="0 0 12 12" fill="none"><path d="M2 4L6 8L10 4" stroke="rgb(136,136,136)" stroke-width="1.5" stroke-linecap="round"/></svg>"##
    };

    rsx! {
        div {
            style: "{container_style}",

            // Label
            if let Some(label_text) = label {
                div {
                    style: "{label_style}",
                    "{label_text}"
                }
            }

            // Button
            div {
                style: "{button_style}",
                onclick: move |_| {
                    let current = *is_open.read();
                    is_open.set(!current);
                },

                span { "{selected_text}" }
                span {
                    dangerous_inner_html: "{arrow_svg}",
                }
            }

            // Dropdown menu
            if *is_open.read() {
                div {
                    style: "{dropdown_style}",

                    for (idx, option) in options.iter().enumerate() {
                        {
                            let is_selected = idx == selected;
                            let style = if is_selected {
                                option_selected_style
                            } else {
                                option_style
                            };
                            let option_text = option.to_string();

                            rsx! {
                                div {
                                    key: "{idx}",
                                    style: "{style}",
                                    onmouseenter: move |_| {
                                        // Could add hover state here
                                    },
                                    onclick: move |_| {
                                        on_change.call(idx);
                                        is_open.set(false);
                                    },
                                    "{option_text}"
                                }
                            }
                        }
                    }
                }

                // Click-outside handler (invisible overlay)
                div {
                    style: "position: fixed; top: 0; left: 0; right: 0; bottom: 0; z-index: 999;",
                    onclick: move |_| {
                        is_open.set(false);
                    },
                }
            }
        }
    }
}
