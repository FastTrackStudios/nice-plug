//! Peak meter widget.

use dioxus_native::prelude::*;
use std::time::Duration;

/// Props for the PeakMeter component.
#[derive(Props, Clone, PartialEq)]
pub struct PeakMeterProps {
    /// The current level in decibels.
    pub level_db: f32,
    /// The minimum level to display (default: -60 dB).
    #[props(default = -60.0)]
    pub min_db: f32,
    /// The maximum level to display (default: 0 dB).
    #[props(default = 0.0)]
    pub max_db: f32,
    /// Whether to display vertically instead of horizontally.
    #[props(default = false)]
    pub vertical: bool,
    /// Optional additional CSS class.
    #[props(default)]
    pub class: String,
}

/// A peak meter that displays audio levels.
///
/// The meter shows the current level with a gradient from green (low)
/// through yellow (medium) to red (high/clipping).
///
/// # Example
///
/// ```ignore
/// use nice_plug_dioxus::prelude::*;
/// use atomic_float::AtomicF32;
/// use std::sync::Arc;
///
/// fn MyEditor(peak_meter: Arc<AtomicF32>) -> Element {
///     let level_db = peak_meter.load(std::sync::atomic::Ordering::Relaxed);
///
///     rsx! {
///         PeakMeter {
///             level_db: level_db,
///             min_db: -60.0,
///             max_db: 0.0,
///         }
///     }
/// }
/// ```
#[component]
pub fn PeakMeter(props: PeakMeterProps) -> Element {
    // Calculate the fill percentage
    let range = props.max_db - props.min_db;
    let normalized = if range > 0.0 {
        ((props.level_db - props.min_db) / range).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let fill_size = format!("{}%", normalized * 100.0);

    let meter_class = if props.vertical {
        format!("peak-meter peak-meter--vertical {}", props.class)
    } else {
        format!("peak-meter {}", props.class)
    };

    let fill_style = if props.vertical {
        format!("height: {}", fill_size)
    } else {
        format!("width: {}", fill_size)
    };

    rsx! {
        div {
            class: "{meter_class}",

            div {
                class: "peak-meter__fill",
                style: "{fill_style}",
            }
        }
    }
}

/// Props for the PeakMeterWithHold component.
#[derive(Props, Clone, PartialEq)]
pub struct PeakMeterWithHoldProps {
    /// The current level in decibels.
    pub level_db: f32,
    /// The minimum level to display (default: -60 dB).
    #[props(default = -60.0)]
    pub min_db: f32,
    /// The maximum level to display (default: 0 dB).
    #[props(default = 0.0)]
    pub max_db: f32,
    /// How long to hold the peak before it starts decaying (in milliseconds).
    #[props(default = 1000)]
    pub hold_time_ms: u64,
    /// Whether to display vertically instead of horizontally.
    #[props(default = false)]
    pub vertical: bool,
    /// Optional additional CSS class.
    #[props(default)]
    pub class: String,
}

/// A peak meter with peak hold functionality.
///
/// This version tracks the peak level and displays a peak indicator
/// that decays over time.
#[component]
pub fn PeakMeterWithHold(props: PeakMeterWithHoldProps) -> Element {
    let mut peak_level = use_signal(|| f32::NEG_INFINITY);
    let mut peak_time = use_signal(std::time::Instant::now);

    let hold_time = Duration::from_millis(props.hold_time_ms);

    // Update peak tracking
    let current_peak = *peak_level.read();
    let time_since_peak = peak_time.read().elapsed();

    if props.level_db > current_peak {
        peak_level.set(props.level_db);
        peak_time.set(std::time::Instant::now());
    } else if time_since_peak > hold_time {
        // Decay the peak
        let decay_rate = 30.0; // dB per second
        let decay = decay_rate * time_since_peak.as_secs_f32();
        let new_peak = (current_peak - decay).max(props.level_db);
        peak_level.set(new_peak);
    }

    // Calculate fill percentages
    let range = props.max_db - props.min_db;
    let level_normalized = if range > 0.0 {
        ((props.level_db - props.min_db) / range).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let peak_normalized = if range > 0.0 {
        ((current_peak - props.min_db) / range).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let fill_size = format!("{}%", level_normalized * 100.0);
    let peak_pos = format!("{}%", peak_normalized * 100.0);

    let meter_class = if props.vertical {
        format!("peak-meter peak-meter--vertical {}", props.class)
    } else {
        format!("peak-meter {}", props.class)
    };

    let fill_style = if props.vertical {
        format!("height: {}", fill_size)
    } else {
        format!("width: {}", fill_size)
    };

    let peak_style = if props.vertical {
        format!("bottom: {}", peak_pos)
    } else {
        format!("left: {}", peak_pos)
    };

    rsx! {
        div {
            class: "{meter_class}",

            div {
                class: "peak-meter__fill",
                style: "{fill_style}",
            }

            if peak_normalized > 0.0 {
                div {
                    class: "peak-meter__peak",
                    style: "{peak_style}",
                }
            }
        }
    }
}
