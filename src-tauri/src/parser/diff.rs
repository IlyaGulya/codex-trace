//! Structural line + word diff, ported from `shared/diff.ts` so the Rust TUI
//! renders the same red/green, word-highlighted diffs as the web/desktop
//! frontend for `apply_patch` tool calls (see `patch.rs`).
//!
//! Produces a line-level unified diff that PRESERVES unchanged context lines,
//! and for each pair of changed lines computes intra-line WORD-level change
//! ranges (LCS pairing + a similarity gate so dissimilar lines aren't falsely
//! aligned). Keep in sync with `shared/diff.ts`.

/// Beyond this many DP cells the O(n*m) line LCS is skipped in favour of a
/// plain "all removed then all added" rendering. Edit payloads are small in
/// practice.
const MAX_LCS_CELLS: usize = 40_000;

/// Minimum fraction of shared non-whitespace tokens for two lines to be
/// treated as an edit of each other (and thus word-diffed rather than shown
/// as wholly removed/added).
const WORD_SIMILARITY_THRESHOLD: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Removed,
    Added,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffSegment {
    pub text: String,
    /// True when this span differs from the paired line (word-level highlight).
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub segments: Vec<DiffSegment>,
}

/// A line classified as context/removed/added, before word-level segmenting.
#[derive(Debug, Clone, PartialEq)]
pub struct LineOp {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Words (letters/digits/underscore), whitespace runs, and punctuation runs.
pub fn tokenize(s: &str) -> Vec<String> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Whitespace,
        Punct,
    }
    fn classify(c: char) -> Class {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Whitespace
        } else {
            Class::Punct
        }
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_class: Option<Class> = None;
    for c in s.chars() {
        let class = classify(c);
        match &current_class {
            Some(cc) if *cc == class => current.push(c),
            _ => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                current.push(c);
                current_class = Some(class);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_whitespace(tok: &str) -> bool {
    tok.trim().is_empty()
}

/// Marks which elements of `a` / `b` participate in their longest common
/// subsequence (by equality). O(n*m) time and space.
fn lcs_matched(a: &[String], b: &[String]) -> (Vec<bool>, Vec<bool>) {
    let n = a.len();
    let m = b.len();
    let mut a_matched = vec![false; n];
    let mut b_matched = vec![false; m];
    if n == 0 || m == 0 {
        return (a_matched, b_matched);
    }

    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            a_matched[i] = true;
            b_matched[j] = true;
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (a_matched, b_matched)
}

/// Merge adjacent tokens with the same changed flag. Whitespace is never
/// flagged changed, so leading/trailing spaces aren't highlighted on their own.
fn build_segments(tokens: &[String], matched: &[bool]) -> Vec<DiffSegment> {
    let mut segs: Vec<DiffSegment> = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        let changed = !matched[i] && !is_whitespace(tok);
        match segs.last_mut() {
            Some(last) if last.changed == changed => last.text.push_str(tok),
            _ => segs.push(DiffSegment {
                text: tok.clone(),
                changed,
            }),
        }
    }
    segs
}

/// Word-level diff of two lines. Returns `None` when the lines are too
/// dissimilar to be considered a single edit (caller then shows them wholly
/// removed/added).
pub fn word_diff(old_line: &str, new_line: &str) -> Option<(Vec<DiffSegment>, Vec<DiffSegment>)> {
    let a = tokenize(old_line);
    let b = tokenize(new_line);
    let (a_matched, b_matched) = lcs_matched(&a, &b);

    let a_non_ws = a.iter().filter(|t| !is_whitespace(t)).count();
    let b_non_ws = b.iter().filter(|t| !is_whitespace(t)).count();
    let denom = a_non_ws.max(b_non_ws);
    if denom == 0 {
        return None;
    }

    let common_non_ws = a
        .iter()
        .enumerate()
        .filter(|(i, t)| a_matched[*i] && !is_whitespace(t))
        .count();
    if (common_non_ws as f64) / (denom as f64) < WORD_SIMILARITY_THRESHOLD {
        return None;
    }

    Some((
        build_segments(&a, &a_matched),
        build_segments(&b, &b_matched),
    ))
}

fn line_diff_ops(old_lines: &[String], new_lines: &[String]) -> Vec<LineOp> {
    let n = old_lines.len();
    let m = new_lines.len();
    if n.saturating_mul(m) > MAX_LCS_CELLS {
        return old_lines
            .iter()
            .map(|t| LineOp {
                kind: DiffLineKind::Removed,
                text: t.clone(),
            })
            .chain(new_lines.iter().map(|t| LineOp {
                kind: DiffLineKind::Added,
                text: t.clone(),
            }))
            .collect();
    }

    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old_lines[i] == new_lines[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            ops.push(LineOp {
                kind: DiffLineKind::Context,
                text: old_lines[i].clone(),
            });
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(LineOp {
                kind: DiffLineKind::Removed,
                text: old_lines[i].clone(),
            });
            i += 1;
        } else {
            ops.push(LineOp {
                kind: DiffLineKind::Added,
                text: new_lines[j].clone(),
            });
            j += 1;
        }
    }
    while i < n {
        ops.push(LineOp {
            kind: DiffLineKind::Removed,
            text: old_lines[i].clone(),
        });
        i += 1;
    }
    while j < m {
        ops.push(LineOp {
            kind: DiffLineKind::Added,
            text: new_lines[j].clone(),
        });
        j += 1;
    }
    ops
}

/// Within each maximal run of changes, emit all removed lines before all
/// added lines so `removed[i]` can be paired with `added[i]` for word-diffing.
pub fn group_runs(ops: Vec<LineOp>) -> Vec<LineOp> {
    let mut out = Vec::with_capacity(ops.len());
    let mut k = 0;
    while k < ops.len() {
        if ops[k].kind == DiffLineKind::Context {
            out.push(ops[k].clone());
            k += 1;
            continue;
        }
        let mut removed = Vec::new();
        let mut added = Vec::new();
        while k < ops.len() && ops[k].kind != DiffLineKind::Context {
            if ops[k].kind == DiffLineKind::Removed {
                removed.push(ops[k].clone());
            } else {
                added.push(ops[k].clone());
            }
            k += 1;
        }
        out.extend(removed);
        out.extend(added);
    }
    out
}

/// Turn grouped line ops into rendered diff lines, pairing `removed[i]` with
/// `added[i]` within each change run for word-level highlighting. Input ops
/// must already be grouped (all removed before all added within each change
/// run) — see `group_runs`.
pub fn segmentize(ops: Vec<LineOp>) -> Vec<DiffLine> {
    let mut result = Vec::new();
    let mut k = 0;
    let n = ops.len();
    while k < n {
        if ops[k].kind == DiffLineKind::Context {
            result.push(DiffLine {
                kind: DiffLineKind::Context,
                segments: vec![DiffSegment {
                    text: ops[k].text.clone(),
                    changed: false,
                }],
            });
            k += 1;
            continue;
        }

        let mut removed = Vec::new();
        while k < n && ops[k].kind == DiffLineKind::Removed {
            removed.push(ops[k].text.clone());
            k += 1;
        }
        let mut added = Vec::new();
        while k < n && ops[k].kind == DiffLineKind::Added {
            added.push(ops[k].text.clone());
            k += 1;
        }

        let pairs = removed.len().min(added.len());
        let wds: Vec<_> = (0..pairs)
            .map(|i| word_diff(&removed[i], &added[i]))
            .collect();

        for (i, text) in removed.iter().enumerate() {
            let segs = if i < pairs {
                wds[i].as_ref().map(|(old, _)| old.clone())
            } else {
                None
            }
            .unwrap_or_else(|| {
                vec![DiffSegment {
                    text: text.clone(),
                    changed: false,
                }]
            });
            result.push(DiffLine {
                kind: DiffLineKind::Removed,
                segments: segs,
            });
        }
        for (i, text) in added.iter().enumerate() {
            let segs = if i < pairs {
                wds[i].as_ref().map(|(_, new)| new.clone())
            } else {
                None
            }
            .unwrap_or_else(|| {
                vec![DiffSegment {
                    text: text.clone(),
                    changed: false,
                }]
            });
            result.push(DiffLine {
                kind: DiffLineKind::Added,
                segments: segs,
            });
        }
    }
    result
}

pub fn compute_edit_diff(old_lines: &[String], new_lines: &[String]) -> Vec<DiffLine> {
    segmentize(group_runs(line_diff_ops(old_lines, new_lines)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn tokenize_splits_words_whitespace_and_punctuation() {
        assert_eq!(
            tokenize("foo_bar(1, 2)"),
            vec!["foo_bar", "(", "1", ",", " ", "2", ")"]
        );
    }

    #[test]
    fn word_diff_highlights_the_changed_token_when_lines_are_similar() {
        let (old, new) = word_diff(r#"    println!("old");"#, r#"    println!("new");"#).unwrap();
        assert!(old.iter().any(|s| s.changed && s.text == "old"));
        assert!(new.iter().any(|s| s.changed && s.text == "new"));
    }

    #[test]
    fn word_diff_returns_none_for_dissimilar_lines() {
        assert!(word_diff("completely different", "totally unrelated text").is_none());
    }

    #[test]
    fn compute_edit_diff_preserves_unchanged_context_lines() {
        let old = lines("fn main() {\n    println!(\"old\");\n}");
        let new = lines("fn main() {\n    println!(\"new\");\n}");
        let diff = compute_edit_diff(&old, &new);
        let kinds: Vec<_> = diff.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::Context,
            ]
        );
    }
}
