//! A small line-level diff used to summarize file edits.
//!
//! Produces a unified-style text diff (`-`/`+`/` ` line prefixes) via a
//! longest-common-subsequence alignment of the two inputs' lines. It is meant
//! for human display, not for machine application — there are no hunk headers.
//!
//! Only [`CONTEXT`] lines of unchanged text are kept around each change; longer
//! runs of unchanged lines collapse into a single `⋯ N unchanged lines` marker,
//! so a one-line edit in a large file shows a small hunk instead of the whole
//! file.

/// Unchanged context lines kept on each side of a change. Runs of equal lines
/// longer than this (beyond the context window) are elided into a gap marker.
const CONTEXT: usize = 3;

/// Render a unified-style diff of `old` → `new`, labelled with `label`.
///
/// Returns an empty string when the contents are identical.
pub fn unified(old: &str, new: &str, label: &str) -> String {
    if old == new {
        return String::new();
    }

    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = lcs_diff(&a, &b);

    // Keep every change, plus up to CONTEXT equal lines on either side of one.
    // Equal lines outside every change's window are dropped and replaced by a
    // single gap marker, bounding the diff to the regions that actually changed.
    let mut keep = vec![false; ops.len()];
    for (i, op) in ops.iter().enumerate() {
        if !matches!(op, Op::Equal(_)) {
            let lo = i.saturating_sub(CONTEXT);
            let hi = (i + CONTEXT + 1).min(ops.len());
            keep[lo..hi].fill(true);
        }
    }

    let mut out = String::new();
    out.push_str(&format!("--- {label}\n+++ {label}\n"));
    let mut i = 0;
    while i < ops.len() {
        if keep[i] {
            out.push(match &ops[i] {
                Op::Equal(_) => ' ',
                Op::Delete(_) => '-',
                Op::Insert(_) => '+',
            });
            out.push_str(ops[i].text());
            out.push('\n');
            i += 1;
            continue;
        }
        // A maximal run of dropped (always unchanged) lines. A lone hidden line
        // costs the same as its marker, so show it; longer runs collapse.
        let start = i;
        while i < ops.len() && !keep[i] {
            i += 1;
        }
        let n = i - start;
        if n == 1 {
            out.push(' ');
            out.push_str(ops[start].text());
            out.push('\n');
        } else {
            out.push_str(&format!("⋯ {n} unchanged lines\n"));
        }
    }
    out
}

enum Op<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

impl<'a> Op<'a> {
    /// The line's text, without its diff prefix.
    fn text(&self) -> &'a str {
        match self {
            Op::Equal(line) | Op::Delete(line) | Op::Insert(line) => line,
        }
    }
}

/// Diff two line sequences via an LCS table and backtrack.
fn lcs_diff<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Op<'a>> {
    let n = a.len();
    let m = b.len();
    // table[i][j] = LCS length of a[i..] and b[j..].
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Equal(a[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push(Op::Delete(a[i]));
            i += 1;
        } else {
            ops.push(Op::Insert(b[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(a[i]));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(b[j]));
        j += 1;
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_empty() {
        assert_eq!(unified("a\nb\n", "a\nb\n", "f"), "");
    }

    #[test]
    fn single_line_change() {
        let d = unified(
            "let x = 1;\nlet y = 3;\n",
            "let x = 2;\nlet y = 3;\n",
            "src/main.rs",
        );
        assert!(d.contains("-let x = 1;"));
        assert!(d.contains("+let x = 2;"));
        assert!(d.contains(" let y = 3;"));
    }

    #[test]
    fn pure_insertion() {
        let d = unified("a\n", "a\nb\n", "f");
        assert!(d.contains(" a"));
        assert!(d.contains("+b"));
        // No content was deleted: no body line begins with '-'.
        assert!(!d.lines().skip(2).any(|l| l.starts_with('-')));
    }

    #[test]
    fn creation_from_empty() {
        let d = unified("", "hello\n", "new.txt");
        assert!(d.contains("+hello"));
    }

    #[test]
    fn elides_unchanged_lines_far_from_the_change() {
        // A one-line change buried in a long file: the diff keeps a small window
        // of context and collapses the rest into a single gap marker, rather
        // than echoing the whole file.
        let mut old = String::new();
        for i in 0..50 {
            old.push_str(&format!("line {i}\n"));
        }
        let new = old.replace("line 25", "line twenty-five");

        let d = unified(&old, &new, "f");
        assert!(d.contains("-line 25"));
        assert!(d.contains("+line twenty-five"));
        // Context is bounded: the change's near neighbours show...
        assert!(d.contains(" line 24"));
        assert!(d.contains(" line 26"));
        // ...but distant lines do not, and a gap marker stands in for them.
        assert!(!d.contains("line 0\n"));
        assert!(d.contains("⋯ "));
        assert!(d.contains("unchanged lines"));
        // The hunk is a small fraction of the file, not all ~50 lines.
        assert!(d.lines().count() < 20, "diff not compact: {d}");
    }

    #[test]
    fn two_nearby_insertions_collapse_the_distant_top() {
        // Mirrors the playwright.config.ts case: a long unchanged preamble, then
        // two insertions a few lines apart near the bottom. The preamble must
        // collapse to a gap marker rather than echoing the whole file.
        let mut old = String::from("// config\nimport x;\n\n");
        for i in 0..30 {
            old.push_str(&format!("setting_{i}: true,\n"));
        }
        old.push_str("ecom: {\n  name: 'ecom',\n}\nhive: {\n  name: 'hive',\n}\n");
        // Insert a line inside each of the two trailing blocks.
        let new = old
            .replace("name: 'ecom',\n", "name: 'ecom',\n  parallel: false,\n")
            .replace("name: 'hive',\n", "name: 'hive',\n  parallel: false,\n");

        let d = unified(&old, &new, "playwright.config.ts");
        assert!(d.contains("+  parallel: false,"));
        // The distant preamble is elided, not echoed line by line.
        assert!(d.contains("⋯ "), "preamble should collapse: {d}");
        assert!(!d.contains("setting_0: true,"), "{d}");
        // The whole diff is far smaller than the ~37-line file.
        assert!(d.lines().count() < 20, "diff not compact: {d}");
    }

    #[test]
    fn keeps_full_context_for_small_diffs() {
        // Nothing to elide: a short file shows every line, no gap marker.
        let d = unified("a\nb\nc\n", "a\nB\nc\n", "f");
        assert!(d.contains(" a"));
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
        assert!(d.contains(" c"));
        assert!(!d.contains("⋯"));
    }
}
