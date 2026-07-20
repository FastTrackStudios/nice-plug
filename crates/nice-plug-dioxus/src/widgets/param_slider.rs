//! Parameter slider widget.

use crate::context::use_param_context;
use dioxus_native::prelude::*;
use nice_plug_core::params::internals::ParamPtr;

/// Props for the ParamSlider component.
#[derive(Props, Clone, PartialEq)]
pub struct ParamSliderProps {
    /// The parameter pointer to control.
    pub param_ptr: ParamPtr,
    /// Optional additional CSS class.
    #[props(default)]
    pub class: String,
}

/// A horizontal slider for controlling a plugin parameter.
///
/// This widget displays the parameter name, a draggable slider track,
/// and the current value with its unit.
///
/// # Example
///
/// ```ignore
/// use nice_plug_dioxus::prelude::*;
///
/// fn MyEditor(params: Arc<MyParams>) -> Element {
///     rsx! {
///         ParamSlider { param_ptr: params.gain.as_ptr() }
///     }
/// }
/// ```
#[component]
pub fn ParamSlider(props: ParamSliderProps) -> Element {
    let ctx = use_param_context();
    let mut is_dragging = use_signal(|| false);
    let mut drag_start_value = use_signal(|| 0.0f32);
    let mut drag_start_y = use_signal(|| 0.0f32);

    // Read current parameter state (these are safe to call on ParamPtr)
    let normalized = unsafe { props.param_ptr.modulated_normalized_value() };
    let display_value = unsafe { props.param_ptr.normalized_value_to_string(normalized, true) };
    let name = unsafe { props.param_ptr.name() };

    let fill_width = format!("{}%", normalized * 100.0);

    let param_ptr = props.param_ptr;

    rsx! {
        div {
            class: "param-slider {props.class}",

            // Parameter name
            div {
                class: "param-slider__label",
                "{name}"
            }

            // Slider track
            div {
                class: "param-slider__track",
                onmousedown: {
                    let ctx = ctx.clone();
                    move |evt: MouseEvent| {
                        is_dragging.set(true);
                        drag_start_value.set(normalized);
                        drag_start_y.set(evt.client_coordinates().y as f32);
                        ctx.begin_set_raw(param_ptr);
                    }
                },
                onmousemove: {
                    let ctx = ctx.clone();
                    move |evt: MouseEvent| {
                        if *is_dragging.read() {
                            // Vertical drag: up increases, down decreases
                            let delta = (drag_start_y() - evt.client_coordinates().y as f32) / 150.0;
                            let new_value = (drag_start_value() + delta).clamp(0.0, 1.0);
                            ctx.set_normalized_raw(param_ptr, new_value);
                        }
                    }
                },
                onmouseup: {
                    let ctx = ctx.clone();
                    move |_| {
                        if *is_dragging.read() {
                            is_dragging.set(false);
                            ctx.end_set_raw(param_ptr);
                        }
                    }
                },
                onmouseleave: {
                    let ctx = ctx.clone();
                    move |_| {
                        if *is_dragging.read() {
                            is_dragging.set(false);
                            ctx.end_set_raw(param_ptr);
                        }
                    }
                },
                // Double-click to reset to default
                ondoubleclick: {
                    let ctx = ctx.clone();
                    move |_| {
                        let default = unsafe { param_ptr.default_normalized_value() };
                        ctx.begin_set_raw(param_ptr);
                        ctx.set_normalized_raw(param_ptr, default);
                        ctx.end_set_raw(param_ptr);
                    }
                },

                // Fill bar
                div {
                    class: "param-slider__fill",
                    style: "width: {fill_width}",
                }

                // Value display overlay
                div {
                    class: "param-slider__value",
                    "{display_value}"
                }
            }
        }
    }
}
