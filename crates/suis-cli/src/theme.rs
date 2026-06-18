//! The Suis colour palette.
//!
//! A single dark theme so every screen and widget draws from one coherent set
//! of colours rather than scattered raw [`Color`] literals. The signature green
//! ([`ACCENT`]) is the input box's colour, carried through the rest of the UI as
//! the primary accent; cooler blues/ambers are reserved for secondary roles so
//! the whole interface reads as one palette.

use ratatui::style::Color;

// --- Surfaces --------------------------------------------------------------

/// The app background: a very dark neutral gray, soft rather than near-black.
pub const BG: Color = Color::Rgb(30, 30, 30);
/// Filled background of a user message card — a faint green-tinted panel.
pub const USER_SURFACE: Color = Color::Rgb(40, 50, 42);
/// Dim border colour for panels, boxes, and the transcript frame.
pub const BORDER: Color = Color::Rgb(48, 54, 61);

// --- Text ------------------------------------------------------------------

/// Bright text for emphasis (selected rows, key details).
pub const TEXT_BRIGHT: Color = Color::Rgb(230, 237, 243);
/// Primary body text.
pub const TEXT: Color = Color::Rgb(201, 209, 217);
/// Secondary / muted text.
pub const TEXT_DIM: Color = Color::Rgb(139, 148, 158);
/// Faint hints, disabled text, and diff headers.
pub const TEXT_FAINT: Color = Color::Rgb(91, 99, 110);

// --- Accents ---------------------------------------------------------------

/// The signature green: the input box, the user, success, and added diff lines.
pub const ACCENT: Color = Color::Rgb(63, 185, 80);
/// Informational cool accent: the agent, titles, and prompts.
pub const INFO: Color = Color::Rgb(88, 166, 255);
/// Attention amber: tasks, warnings, and the permission dialog.
pub const WARN: Color = Color::Rgb(214, 164, 76);
/// Destructive red: errors and removed diff lines.
pub const DANGER: Color = Color::Rgb(248, 81, 73);
/// Secondary violet accent: tool output.
pub const TOOL: Color = Color::Rgb(188, 140, 255);

// --- Code ------------------------------------------------------------------

/// Background of fenced code blocks and inline code spans.
pub const CODE_BG: Color = Color::Rgb(22, 27, 34);
/// Foreground of code text: a soft light blue that reads as "code" even on
/// terminals that drop the background colour.
pub const CODE_FG: Color = Color::Rgb(121, 192, 255);
