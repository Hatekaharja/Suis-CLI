//! The `/usage` detail popup.
//!
//! A centered overlay listing this session's token spend per provider, split
//! into sent (prompt) and received (completion) with a total. The footer shows
//! only the active provider's total; this is the full breakdown, and the place
//! to read usage when the footer is hidden on a narrow terminal.

use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::state::ProviderUsage;
use crate::theme;
use crate::widgets::context_gauge::fmt_tokens;
use crate::widgets::permission_prompt::centered_rect;

/// Width of each right-aligned number column (`sent`/`recv`/`total`).
const NUM_COL: usize = 8;
/// Cap on the provider-name column so one long id can't push the numbers off.
const NAME_COL_CAP: usize = 24;

/// Render the usage popup for `usage` centered over `area`.
pub fn render(frame: &mut Frame, area: Rect, usage: &HashMap<String, ProviderUsage>) {
    let popup = centered_rect(60, 50, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            "Token Usage",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines(usage)).block(block), popup);
}

/// Build the popup body: a header row, one row per provider (sorted by name),
/// and a totals row — or an empty-state line before anything is counted.
fn lines(usage: &HashMap<String, ProviderUsage>) -> Vec<Line<'static>> {
    if usage.is_empty() {
        return vec![Line::from(Span::styled(
            "No tokens used yet this session.",
            Style::default().fg(theme::TEXT_FAINT),
        ))];
    }

    let name_col = usage
        .keys()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(0)
        .clamp("provider".len(), NAME_COL_CAP);

    let mut providers: Vec<(&String, &ProviderUsage)> = usage.iter().collect();
    providers.sort_by(|a, b| a.0.cmp(b.0));

    let mut out = vec![header_row(name_col)];
    let (mut sent, mut received) = (0usize, 0usize);
    for (name, u) in providers {
        sent += u.sent;
        received += u.received;
        out.push(data_row(
            name,
            name_col,
            u.sent,
            u.received,
            u.total(),
            theme::TEXT,
            false,
        ));
    }
    out.push(Line::from(""));
    out.push(data_row(
        "total",
        name_col,
        sent,
        received,
        sent + received,
        theme::ACCENT,
        true,
    ));
    out
}

fn header_row(name_col: usize) -> Line<'static> {
    let style = Style::default()
        .fg(theme::TEXT_DIM)
        .add_modifier(Modifier::BOLD);
    Line::from(Span::styled(
        format!(
            "{:<name_col$}  {:>NUM_COL$}  {:>NUM_COL$}  {:>NUM_COL$}",
            "provider", "sent", "recv", "total"
        ),
        style,
    ))
}

#[allow(clippy::too_many_arguments)]
fn data_row(
    name: &str,
    name_col: usize,
    sent: usize,
    received: usize,
    total: usize,
    color: ratatui::style::Color,
    bold: bool,
) -> Line<'static> {
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    // Truncate an over-long name to the column so the numbers stay aligned.
    let name: String = name.chars().take(name_col).collect();
    Line::from(Span::styled(
        format!(
            "{:<name_col$}  {:>NUM_COL$}  {:>NUM_COL$}  {:>NUM_COL$}",
            name,
            fmt_tokens(sent),
            fmt_tokens(received),
            fmt_tokens(total)
        ),
        style,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_usage_shows_empty_state() {
        let flat = flat(&lines(&HashMap::new()));
        assert!(flat.contains("No tokens used yet"));
    }

    #[test]
    fn lists_providers_with_totals() {
        let mut usage = HashMap::new();
        usage.insert(
            "Ollama".to_string(),
            ProviderUsage {
                sent: 12_000,
                received: 3_000,
            },
        );
        usage.insert(
            "OpenAI".to_string(),
            ProviderUsage {
                sent: 1_000,
                received: 200,
            },
        );
        let flat = flat(&lines(&usage));
        assert!(flat.contains("Ollama"));
        assert!(flat.contains("OpenAI"));
        // Per-provider totals and the grand total are formatted compactly.
        assert!(flat.contains("15k"), "Ollama total: {flat}");
        assert!(flat.contains("total"));
        // Grand total: 12k+3k+1k+0.2k = 16.2k.
        assert!(flat.contains("16.2k"), "grand total: {flat}");
    }
}
