//! The context-pressure indicator.
//!
//! A small, approximate gauge of how much of the token budget the conversation
//! is using, so the user sees pressure building before mechanical pruning kicks
//! in (transparency over surprise). The estimate is chars/4 — labelled as
//! approximate — and recomputed at each turn boundary from the agent's
//! [`AgentEvent::ContextUsage`](suis_agent::AgentEvent::ContextUsage).

/// The fraction of the budget past which the gauge switches to a warning style.
const WARN_FRACTION: f32 = 0.80;

/// Format a token count compactly for the UI: `999`, `12.3k`, `48k`, `1.2M`.
/// A trailing `.0` is dropped so round thousands read as `48k`, not `48.0k`.
/// Shared by the input border, the footer, and the `/usage` popup.
pub fn fmt_tokens(n: usize) -> String {
    let scaled = |value: f64, suffix: &str| -> String {
        let s = format!("{value:.1}");
        let s = s.strip_suffix(".0").map(str::to_string).unwrap_or(s);
        format!("{s}{suffix}")
    };
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        scaled(n as f64 / 1_000.0, "k")
    } else {
        scaled(n as f64 / 1_000_000.0, "M")
    }
}

/// A snapshot of context usage for one turn boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextGauge {
    /// Estimated tokens the last assembled request occupied.
    pub used: usize,
    /// The token budget history is pruned against.
    pub budget: usize,
    /// Whether mechanical pruning acted on the last request.
    pub pruned: bool,
}

impl ContextGauge {
    /// Build a gauge from the raw usage numbers.
    pub fn new(used: usize, budget: usize, pruned: bool) -> Self {
        ContextGauge {
            used,
            budget,
            pruned,
        }
    }

    /// Usage as a fraction of the budget (0.0–…; may exceed 1.0 when the
    /// protected tail alone is over budget). A zero budget reads as 0.
    pub fn fraction(&self) -> f32 {
        if self.budget == 0 {
            0.0
        } else {
            self.used as f32 / self.budget as f32
        }
    }

    /// Usage as a whole-number percentage, clamped to 0–999 for display.
    pub fn percent(&self) -> u16 {
        (self.fraction() * 100.0).round().clamp(0.0, 999.0) as u16
    }

    /// Whether usage has crossed the warning threshold.
    pub fn is_warning(&self) -> bool {
        self.fraction() >= WARN_FRACTION
    }

    /// The status-line label, e.g. `ctx 62%` or `ctx 95% pruned`.
    pub fn label(&self) -> String {
        let mut label = format!("ctx {}%", self.percent());
        if self.pruned {
            label.push_str(" pruned");
        }
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_at_zero() {
        let g = ContextGauge::new(0, 12_000, false);
        assert_eq!(g.percent(), 0);
        assert_eq!(g.label(), "ctx 0%");
        assert!(!g.is_warning());
    }

    #[test]
    fn formats_mid_range() {
        let g = ContextGauge::new(7_440, 12_000, false);
        assert_eq!(g.percent(), 62);
        assert_eq!(g.label(), "ctx 62%");
        assert!(!g.is_warning());
    }

    #[test]
    fn warns_past_eighty_percent() {
        let g = ContextGauge::new(10_000, 12_000, false);
        assert_eq!(g.percent(), 83);
        assert!(g.is_warning());
    }

    #[test]
    fn shows_pruned_state() {
        let g = ContextGauge::new(12_500, 12_000, true);
        assert_eq!(g.percent(), 104);
        assert!(g.is_warning());
        assert_eq!(g.label(), "ctx 104% pruned");
    }

    #[test]
    fn fmt_tokens_is_compact_and_trims_round_thousands() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_234), "1.2k");
        assert_eq!(fmt_tokens(12_300), "12.3k");
        assert_eq!(fmt_tokens(48_000), "48k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn zero_budget_reads_as_zero() {
        let g = ContextGauge::new(500, 0, false);
        assert_eq!(g.percent(), 0);
        assert!(!g.is_warning());
    }
}
