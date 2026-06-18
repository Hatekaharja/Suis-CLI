//! The empty-transcript welcome banner.
//!
//! Rendered in the transcript area when the conversation is empty: the Suis
//! logo (or a one-line wordmark when the terminal is too narrow for it), the
//! session identity — workspace, model and provider, mode — and a few hint
//! lines. Render-only: it is not a message, so the first real message replaces
//! it and `/clear` brings it back.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;

/// The block-glyph logo mark, embedded at compile time.
const LOGO: &str = include_str!("../../../../LOGO.txt");

/// What the banner says about the session.
pub struct Identity<'a> {
    /// Workspace root path.
    pub workspace: Option<&'a str>,
    /// Active model's display name.
    pub model: Option<&'a str>,
    /// Active provider's display name.
    pub provider: Option<&'a str>,
    /// The runtime mode's label (`AGENT`, `PLAN`, `CHAT`).
    pub mode: &'a str,
}

/// Render the banner into the transcript area, framed exactly like the
/// message list so the chrome doesn't shift when the conversation starts.
pub fn render(frame: &mut Frame, area: Rect, identity: &Identity) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            "Suis",
            Style::default()
                .fg(theme::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    let mut lines = banner_lines(identity, inner.width);

    // Center the banner vertically; the paragraph centers it horizontally.
    let pad = (inner.height as usize).saturating_sub(lines.len()) / 2;
    for _ in 0..pad {
        lines.insert(0, Line::from(""));
    }
    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

/// The banner's content: logo or wordmark (gated on `width` so the logo never
/// overflows), identity lines, and the hints.
fn banner_lines(identity: &Identity, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let logo_width = LOGO.lines().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    if width >= logo_width {
        for raw in LOGO.lines() {
            lines.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(theme::ACCENT),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "suis",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )));
    }

    // A Cartesian wink under the wordmark: the line varies by mode, with a few
    // calendar days overriding every mode.
    lines.push(Line::from(Span::styled(
        tagline(identity.mode, current_month_day()),
        Style::default()
            .fg(theme::TEXT_DIM)
            .add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));

    if let Some(model) = identity.model {
        let provider = identity.provider.unwrap_or("?");
        lines.push(identity_line("model", &format!("{model} @ {provider}")));
    }
    if let Some(workspace) = identity.workspace {
        lines.push(identity_line("workspace", workspace));
    }
    lines.push(identity_line("mode", identity.mode));
    lines.push(Line::from(""));

    for hint in [
        "/help for commands",
        "Shift+Tab to switch modes",
        "/model to change models",
    ] {
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(theme::TEXT_FAINT),
        )));
    }
    lines
}

fn identity_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label} · "), Style::default().fg(theme::TEXT_DIM)),
        Span::styled(value.to_string(), Style::default().fg(theme::TEXT)),
    ])
}

/// The tagline shown under the wordmark. A handful of calendar days (as
/// `(month, day)`) override every mode; otherwise the line tracks the active
/// mode. `mode` is the label from [`Mode::label`](crate::Mode), e.g. `"AGENT"`.
fn tagline(mode: &str, today: (u32, u32)) -> &'static str {
    match today {
        (5, 8) => "Je mords, donc je suis.",
        (5, 9) => "Je bâtis, donc je suis.",
        (7, 21) => "Je persiste, donc je suis.",
        _ => match mode {
            "PLAN" => "Je pense, donc je suis.",
            "CHAT" => "Je pige, donc je suis.",
            // AGENT and any unexpected label fall back to the working line.
            _ => "Je bosse, donc je suis.",
        },
    }
}

/// Today's `(month, day)` in the system's local timezone, derived from the
/// system clock. On unix the offset comes from `localtime_r`; everywhere else
/// (and if that call fails) it falls back to UTC, where a few hours' skew at the
/// date boundary is acceptable for a cosmetic line.
fn current_month_day() -> (u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    local_month_day(secs).unwrap_or_else(|| {
        let (_, month, day) = civil_from_days(secs.div_euclid(86_400));
        (month, day)
    })
}

/// `(month, day)` for `secs` (Unix seconds) in local time via `localtime_r`,
/// or `None` if the conversion fails. `localtime_r` consults the system
/// timezone (`TZ`/`/etc/localtime`) and is reentrant, so it is safe to call
/// from the render path.
#[cfg(unix)]
fn local_month_day(secs: i64) -> Option<(u32, u32)> {
    let time = secs as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `time` is a valid pointer to a `time_t`; `localtime_r` fully
    // initializes the `tm` it writes through `tm.as_mut_ptr()` and returns that
    // same pointer (or null on failure), which we check before reading.
    let result = unsafe { libc::localtime_r(&time, tm.as_mut_ptr()) };
    if result.is_null() {
        return None;
    }
    let tm = unsafe { tm.assume_init() };
    Some(((tm.tm_mon + 1) as u32, tm.tm_mday as u32))
}

#[cfg(not(unix))]
fn local_month_day(_secs: i64) -> Option<(u32, u32)> {
    None
}

/// Convert a count of days since the Unix epoch into a `(year, month, day)`
/// civil date (proleptic Gregorian, UTC). Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagline_tracks_mode_off_the_special_days() {
        let ordinary = (3, 14);
        assert_eq!(tagline("PLAN", ordinary), "Je pense, donc je suis.");
        assert_eq!(tagline("AGENT", ordinary), "Je bosse, donc je suis.");
        assert_eq!(tagline("CHAT", ordinary), "Je pige, donc je suis.");
    }

    #[test]
    fn special_days_override_every_mode() {
        for mode in ["PLAN", "AGENT", "CHAT"] {
            assert_eq!(tagline(mode, (5, 8)), "Je mords, donc je suis.");
            assert_eq!(tagline(mode, (5, 9)), "Je bâtis, donc je suis.");
            assert_eq!(tagline(mode, (7, 21)), "Je persiste, donc je suis.");
        }
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01 is 11017 days after the epoch.
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        // A specific override day, round-tripped from a known offset.
        assert_eq!(civil_from_days(20_581).1, 5); // 2026-05-08 → May
        assert_eq!(civil_from_days(20_581), (2026, 5, 8));
    }
}
