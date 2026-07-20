//! Event translation from baseview to blitz.

use baseview::{Event, MouseButton, MouseEvent, ScrollDelta, WindowEvent};
use blitz_traits::events::{
    BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, BlitzWheelEvent, KeyState,
    MouseEventButton, MouseEventButtons, PointerCoords, PointerDetails, UiEvent,
};
use smol_str::SmolStr;

// Re-export Modifiers from keyboard_types (0.6, matching baseview)
// This is the type we use in window.rs for tracking modifier state
pub use keyboard_types::Modifiers;

// Alias for the 0.7 types from blitz-traits (via dioxus re-export)
type BlitzModifiers = dioxus_native::prelude::Modifiers;
type BlitzKey = dioxus_native::prelude::Key;
type BlitzCode = dioxus_native::prelude::Code;
type BlitzLocation = dioxus_native::prelude::Location;

/// Translate a baseview event to a blitz UiEvent.
///
/// `viewport_size` is `(width, height)` in physical pixels. When a mouse button
/// is held (drag in progress), coordinates are clamped to the viewport so that
/// Blitz hit-testing still finds the overlay element even when the OS cursor is
/// outside the plugin window.
pub fn translate_event(
    event: &Event,
    mouse_pos: &mut (f32, f32),
    mouse_buttons: &mut MouseEventButtons,
    modifiers: &mut Modifiers,
    viewport_size: (u32, u32),
) -> Option<UiEvent> {
    match event {
        Event::Mouse(mouse_event) => translate_mouse_event(
            mouse_event,
            mouse_pos,
            mouse_buttons,
            modifiers,
            viewport_size,
        ),
        Event::Keyboard(keyboard_event) => {
            // Update modifier tracking
            *modifiers = keyboard_event.modifiers;
            translate_keyboard_event(keyboard_event)
        }
        Event::Window(WindowEvent::Focused) => None,
        Event::Window(WindowEvent::Unfocused) => None,
        Event::Window(WindowEvent::Resized(_)) => None, // Handled separately
        Event::Window(WindowEvent::WillClose) => None,
    }
}

fn translate_mouse_event(
    event: &MouseEvent,
    mouse_pos: &mut (f32, f32),
    mouse_buttons: &mut MouseEventButtons,
    _modifiers: &Modifiers,
    viewport_size: (u32, u32),
) -> Option<UiEvent> {
    // When a mouse button is held, clamp coordinates to the viewport so that
    // Blitz hit-testing still routes the event to the correct element (e.g. a
    // full-viewport drag overlay). Without this, out-of-bounds coordinates
    // cause hit() to return None and the drag event is lost.
    let clamp = |x: f32, y: f32, buttons: &MouseEventButtons| -> (f32, f32) {
        if buttons.is_empty() {
            (x, y)
        } else {
            let max_x = (viewport_size.0 as f32 - 1.0).max(0.0);
            let max_y = (viewport_size.1 as f32 - 1.0).max(0.0);
            (x.clamp(0.0, max_x), y.clamp(0.0, max_y))
        }
    };

    match event {
        MouseEvent::CursorMoved {
            position,
            modifiers: mods,
        } => {
            let (cx, cy) = clamp(position.x as f32, position.y as f32, mouse_buttons);
            mouse_pos.0 = cx;
            mouse_pos.1 = cy;
            Some(UiEvent::PointerMove(BlitzPointerEvent {
                id: BlitzPointerId::Mouse,
                is_primary: true,
                coords: pointer_coords(mouse_pos.0, mouse_pos.1),
                button: MouseEventButton::Main,
                buttons: *mouse_buttons,
                mods: convert_modifiers(*mods),
                details: PointerDetails::default(),
                element: Default::default(),
                active_pointers: Default::default(),
            }))
        }
        MouseEvent::ButtonPressed {
            button,
            modifiers: mods,
        } => {
            let blitz_button = translate_mouse_button(*button);
            *mouse_buttons |= MouseEventButtons::from(blitz_button);
            Some(UiEvent::PointerDown(BlitzPointerEvent {
                id: BlitzPointerId::Mouse,
                is_primary: true,
                coords: pointer_coords(mouse_pos.0, mouse_pos.1),
                button: blitz_button,
                buttons: *mouse_buttons,
                mods: convert_modifiers(*mods),
                details: PointerDetails::default(),
                element: Default::default(),
                active_pointers: Default::default(),
            }))
        }
        MouseEvent::ButtonReleased {
            button,
            modifiers: mods,
        } => {
            let blitz_button = translate_mouse_button(*button);
            *mouse_buttons &= !MouseEventButtons::from(blitz_button);
            Some(UiEvent::PointerUp(BlitzPointerEvent {
                id: BlitzPointerId::Mouse,
                is_primary: true,
                coords: pointer_coords(mouse_pos.0, mouse_pos.1),
                button: blitz_button,
                buttons: *mouse_buttons,
                mods: convert_modifiers(*mods),
                details: PointerDetails::default(),
                element: Default::default(),
                active_pointers: Default::default(),
            }))
        }
        MouseEvent::WheelScrolled {
            delta,
            modifiers: mods,
        } => {
            let blitz_delta = match delta {
                ScrollDelta::Lines { x, y } => BlitzWheelDelta::Lines(*x as f64, *y as f64),
                ScrollDelta::Pixels { x, y } => BlitzWheelDelta::Pixels(*x as f64, *y as f64),
            };
            Some(UiEvent::Wheel(BlitzWheelEvent {
                delta: blitz_delta,
                coords: pointer_coords(mouse_pos.0, mouse_pos.1),
                buttons: *mouse_buttons,
                mods: convert_modifiers(*mods),
                element: Default::default(),
            }))
        }
        MouseEvent::CursorEntered => None,
        MouseEvent::CursorLeft => None,
        // Drag events - not currently translated to blitz events
        MouseEvent::DragEntered { .. } => None,
        MouseEvent::DragMoved { .. } => None,
        MouseEvent::DragLeft => None,
        MouseEvent::DragDropped { .. } => None,
    }
}

/// Fill all six page/screen/client coordinates with the same pair. We don't
/// have separate page vs screen vs client in baseview, so they collapse.
#[inline]
fn pointer_coords(x: f32, y: f32) -> PointerCoords {
    PointerCoords {
        page_x: x,
        page_y: y,
        screen_x: x,
        screen_y: y,
        client_x: x,
        client_y: y,
    }
}

fn translate_keyboard_event(event: &keyboard_types::KeyboardEvent) -> Option<UiEvent> {
    let key = convert_key(&event.key);
    let code = convert_code(&event.code);
    let modifiers = convert_modifiers(event.modifiers);
    let location = convert_location(event.location);

    // Extract text for character keys
    let text = match &event.key {
        keyboard_types::Key::Character(s) => Some(SmolStr::new(s)),
        _ => None,
    };

    let blitz_event = BlitzKeyEvent {
        key,
        code,
        modifiers,
        location,
        is_auto_repeating: event.repeat,
        is_composing: event.is_composing,
        state: convert_key_state(event.state),
        text,
    };

    match event.state {
        keyboard_types::KeyState::Down => Some(UiEvent::KeyDown(blitz_event)),
        keyboard_types::KeyState::Up => Some(UiEvent::KeyUp(blitz_event)),
    }
}

fn translate_mouse_button(button: MouseButton) -> MouseEventButton {
    match button {
        MouseButton::Left => MouseEventButton::Main,
        MouseButton::Right => MouseEventButton::Secondary,
        MouseButton::Middle => MouseEventButton::Auxiliary,
        MouseButton::Back => MouseEventButton::Fourth,
        MouseButton::Forward => MouseEventButton::Fifth,
        MouseButton::Other(_) => MouseEventButton::Main,
    }
}

/// Convert baseview/keyboard_types 0.6 Modifiers to blitz-traits/keyboard_types 0.7 Modifiers.
/// Both types have the same bitflags values, so we can convert via the underlying bits.
fn convert_modifiers(mods: keyboard_types::Modifiers) -> BlitzModifiers {
    // The bitflags have the same values in both versions:
    // ALT = 1, CONTROL = 2, SHIFT = 4, META = 8, etc.
    BlitzModifiers::from_bits_truncate(mods.bits())
}

/// Convert keyboard_types 0.6 Key to 0.7 Key via string round-trip.
/// Both versions have identical variant names and string representations.
fn convert_key(key: &keyboard_types::Key) -> BlitzKey {
    match key {
        keyboard_types::Key::Character(s) => BlitzKey::Character(s.to_string()),
        other => {
            let s = other.to_string();
            s.parse::<BlitzKey>().unwrap_or(BlitzKey::Unidentified)
        }
    }
}

/// Convert keyboard_types 0.6 Code to 0.7 Code via string round-trip.
fn convert_code(code: &keyboard_types::Code) -> BlitzCode {
    let s = format!("{:?}", code);
    s.parse::<BlitzCode>().unwrap_or(BlitzCode::Unidentified)
}

/// Convert keyboard_types 0.6 Location to 0.7 Location via discriminant.
/// Both versions use the same integer discriminants (0-3).
fn convert_location(loc: keyboard_types::Location) -> BlitzLocation {
    match loc as u32 {
        0 => BlitzLocation::Standard,
        1 => BlitzLocation::Left,
        2 => BlitzLocation::Right,
        3 => BlitzLocation::Numpad,
        _ => BlitzLocation::Standard,
    }
}

/// Convert keyboard_types 0.6 KeyState to blitz-traits KeyState.
fn convert_key_state(state: keyboard_types::KeyState) -> KeyState {
    match state {
        keyboard_types::KeyState::Down => KeyState::Pressed,
        keyboard_types::KeyState::Up => KeyState::Released,
    }
}
