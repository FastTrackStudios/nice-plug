//! Pre-built widgets for common plugin UI elements.
//!
//! These widgets integrate with nice-plug's parameter system through the
//! `ParamContext` provided by `create_dioxus_editor`.

mod dropdown;
mod param_slider;
mod peak_meter;
mod resize_handle;

pub use dropdown::Dropdown;
pub use param_slider::ParamSlider;
pub use peak_meter::PeakMeter;
pub use resize_handle::ResizeHandle;
