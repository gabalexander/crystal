//! Handing the mouse on to a program that asked for it: turning a mouse
//! event into the bytes a terminal would send the program, in the way the
//! program asked for them.
//!
//! A program asks by setting modes on its terminal: which events it wants
//! (presses and releases, drags too, or every move) and how they're to be
//! written (the old one-byte-per-number way, its UTF-8 version, or the SGR
//! way that most programs ask for today).

use crate::vt::{self, mode};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

/// Which mouse events a program asked to hear about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// How a program asked for mouse events to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Default,
    Utf8,
    Sgr,
}

/// What a program asked for the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Protocol {
    pub mode: Mode,
    pub encoding: Encoding,
}

impl Protocol {
    /// What the program on `screen` asked for, or `None` when it hasn't
    /// asked for the mouse. Of the modes it set, the one that hears the
    /// most counts.
    pub fn of(screen: &vt::Screen) -> Option<Protocol> {
        let mode = if screen.mode(mode::MOUSE_ANY) {
            Mode::AnyMotion
        } else if screen.mode(mode::MOUSE_BUTTON) {
            Mode::ButtonMotion
        } else if screen.mode(mode::MOUSE_NORMAL) {
            Mode::PressRelease
        } else {
            return None;
        };
        let encoding = if screen.mode(mode::MOUSE_SGR) {
            Encoding::Sgr
        } else if screen.mode(mode::MOUSE_UTF8) {
            Encoding::Utf8
        } else {
            Encoding::Default
        };
        Some(Protocol { mode, encoding })
    }
}

/// The bytes for a mouse event at `cell`, a `(row, column)` on the
/// program's screen counted from 0, or `None` when the program didn't ask
/// for this kind of event, or its way of writing them can't say where
/// the event was.
pub fn encode(
    kind: MouseEventKind,
    modifiers: KeyModifiers,
    cell: (u16, u16),
    protocol: Protocol,
) -> Option<Vec<u8>> {
    let report = report(kind, protocol.mode)?;
    let button = report.button + modifier_bits(modifiers);
    // The protocols count rows and columns from 1.
    let (row, column) = cell;
    let x = u32::from(column) + 1;
    let y = u32::from(row) + 1;
    match protocol.encoding {
        Encoding::Sgr => {
            let end = if report.release { 'm' } else { 'M' };
            Some(format!("\x1b[<{button};{x};{y}{end}").into_bytes())
        }
        Encoding::Default => {
            let button = if report.release {
                RELEASE + modifier_bits(modifiers)
            } else {
                button
            };
            let mut bytes = b"\x1b[M".to_vec();
            for number in [button, x, y] {
                // Each number is a byte of its own, 32 past the number, so
                // nothing past 255 can be written.
                bytes.push(u8::try_from(32 + number).ok()?);
            }
            Some(bytes)
        }
        Encoding::Utf8 => {
            let button = if report.release {
                RELEASE + modifier_bits(modifiers)
            } else {
                button
            };
            let mut text = String::from("\x1b[M");
            for number in [button, x, y] {
                // The same, but as a character, which reaches further.
                text.push(char::from_u32(32 + number)?);
            }
            Some(text.into_bytes())
        }
    }
}

/// What the two older ways of writing say for a release, since they don't
/// say which button it was.
const RELEASE: u32 = 3;

/// What the protocol adds to a button for a drag: the mouse moving while
/// it's held.
const MOTION: u32 = 32;

/// A mouse event as the protocols see it.
struct Report {
    button: u32,
    release: bool,
}

/// How the protocols see `kind`, if the program asked to hear about it.
fn report(kind: MouseEventKind, mode: Mode) -> Option<Report> {
    let drags = matches!(mode, Mode::ButtonMotion | Mode::AnyMotion);
    let press = |button| Report {
        button,
        release: false,
    };
    match kind {
        MouseEventKind::Down(button) => Some(press(button_number(button))),
        MouseEventKind::Up(button) => Some(Report {
            button: button_number(button),
            release: true,
        }),
        MouseEventKind::Drag(button) if drags => Some(press(button_number(button) + MOTION)),
        // The wheel is two more buttons, which only ever press.
        MouseEventKind::ScrollUp => Some(press(64)),
        MouseEventKind::ScrollDown => Some(press(65)),
        _ => None,
    }
}

fn button_number(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// What the keys held add to the button: 4 for Shift, 8 for Alt, 16 for
/// Ctrl.
fn modifier_bits(modifiers: KeyModifiers) -> u32 {
    let mut bits = 0;
    if modifiers.contains(KeyModifiers::SHIFT) {
        bits += 4;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        bits += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        bits += 16;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT_DOWN: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
    const LEFT_UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
    const LEFT_DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);
    const NONE: KeyModifiers = KeyModifiers::NONE;

    fn sgr(kind: MouseEventKind, modifiers: KeyModifiers, cell: (u16, u16)) -> Option<String> {
        let bytes = encode(
            kind,
            modifiers,
            cell,
            Protocol {
                mode: Mode::ButtonMotion,
                encoding: Encoding::Sgr,
            },
        )?;
        Some(String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn sgr_says_the_button_and_where_counting_from_one() {
        assert_eq!(sgr(LEFT_DOWN, NONE, (1, 2)).unwrap(), "\x1b[<0;3;2M");
        assert_eq!(sgr(LEFT_UP, NONE, (1, 2)).unwrap(), "\x1b[<0;3;2m");
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Right), NONE, (0, 0)).unwrap(),
            "\x1b[<2;1;1M"
        );
    }

    #[test]
    fn a_drag_and_the_wheel_are_buttons_of_their_own() {
        assert_eq!(sgr(LEFT_DRAG, NONE, (4, 9)).unwrap(), "\x1b[<32;10;5M");
        assert_eq!(
            sgr(MouseEventKind::ScrollUp, NONE, (0, 0)).unwrap(),
            "\x1b[<64;1;1M"
        );
        assert_eq!(
            sgr(MouseEventKind::ScrollDown, NONE, (0, 0)).unwrap(),
            "\x1b[<65;1;1M"
        );
    }

    #[test]
    fn keys_held_add_to_the_button() {
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(sgr(LEFT_DOWN, ctrl_shift, (0, 0)).unwrap(), "\x1b[<20;1;1M");
    }

    #[test]
    fn the_old_way_writes_each_number_as_a_byte() {
        let encode = |kind| {
            encode(
                kind,
                NONE,
                (1, 2),
                Protocol {
                    mode: Mode::PressRelease,
                    encoding: Encoding::Default,
                },
            )
        };
        assert_eq!(encode(LEFT_DOWN).unwrap(), b"\x1b[M\x20\x23\x22");
        // A release doesn't say which button.
        assert_eq!(encode(LEFT_UP).unwrap(), b"\x1b[M\x23\x23\x22");
    }

    #[test]
    fn the_old_way_cant_reach_far_columns_but_utf8_can() {
        let far = (0, 300);
        let old = encode(
            LEFT_DOWN,
            NONE,
            far,
            Protocol {
                mode: Mode::PressRelease,
                encoding: Encoding::Default,
            },
        );
        assert_eq!(old, None);

        let utf8 = encode(
            LEFT_DOWN,
            NONE,
            far,
            Protocol {
                mode: Mode::PressRelease,
                encoding: Encoding::Utf8,
            },
        )
        .unwrap();
        let expected: String = [
            '\x1b',
            '[',
            'M',
            ' ',
            char::from_u32(32 + 301).unwrap(),
            '!',
        ]
        .iter()
        .collect();
        assert_eq!(utf8, expected.into_bytes());
    }

    #[test]
    fn a_program_hears_only_what_it_asked_for() {
        let heard = |kind, mode| {
            let protocol = Protocol {
                mode,
                encoding: Encoding::Sgr,
            };
            encode(kind, NONE, (0, 0), protocol).is_some()
        };
        assert!(heard(LEFT_DOWN, Mode::PressRelease));
        assert!(heard(LEFT_UP, Mode::PressRelease));
        assert!(!heard(LEFT_DRAG, Mode::PressRelease));
        assert!(heard(LEFT_DRAG, Mode::ButtonMotion));
        assert!(!heard(MouseEventKind::Moved, Mode::ButtonMotion));
    }

    #[test]
    fn the_protocol_is_read_off_the_programs_modes() {
        let protocol = |output: &[u8]| {
            let mut screen = vt::Screen::new(2, 10);
            screen.process(output);
            Protocol::of(&screen)
        };
        assert_eq!(protocol(b""), None);
        assert_eq!(
            protocol(b"\x1b[?1000h\x1b[?1006h"),
            Some(Protocol {
                mode: Mode::PressRelease,
                encoding: Encoding::Sgr,
            })
        );
        assert_eq!(
            protocol(b"\x1b[?1002h\x1b[?1005h"),
            Some(Protocol {
                mode: Mode::ButtonMotion,
                encoding: Encoding::Utf8,
            })
        );
        assert_eq!(protocol(b"\x1b[?1003h\x1b[?1003l"), None);
    }
}
