//! Parse a Codex `apply_patch` body into per-file, per-hunk structured diffs.
//! Ported from `shared/patch.ts` — keep the two in sync.
//!
//! The patch text Codex logs in a `custom_tool_call` `input` looks like:
//!
//! ```text
//! *** Begin Patch
//! *** Update File: path/to/file
//! @@ optional context heading
//!  unchanged line   (leading space)
//! -removed line
//! +added line
//! *** End Patch
//! ```
//!
//! We honour the patch's own +/-/context classification (rather than
//! re-diffing) and reuse `group_runs` + `segmentize` from `diff.rs` to add
//! word-level highlighting on each paired removed/added run.

use super::diff::{group_runs, segmentize, DiffLine, DiffLineKind, LineOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchFileOp {
    Add,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatchHunk {
    /// Text after the `@@` marker, if any (empty for the implicit first hunk).
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatchFile {
    pub path: String,
    pub op: PatchFileOp,
    /// Destination path when the patch renames/moves the file (`*** Move to:`).
    pub move_path: Option<String>,
    pub hunks: Vec<PatchHunk>,
}

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const UPDATE: &str = "*** Update File: ";
const DELETE: &str = "*** Delete File: ";
const MOVE: &str = "*** Move to: ";

fn looks_like_patch(patch: &str) -> bool {
    patch.contains(BEGIN) || patch.contains(ADD) || patch.contains(UPDATE) || patch.contains(DELETE)
}

/// Parse a Codex apply_patch body. Returns `None` when the text isn't a
/// recognised patch (callers then fall back to rendering it as raw text).
pub fn parse_apply_patch(patch: &str) -> Option<Vec<PatchFile>> {
    if !looks_like_patch(patch) {
        return None;
    }

    let mut lines: Vec<&str> = patch.split('\n').collect();
    // Drop a single trailing empty element from a terminal newline so it
    // doesn't surface as a spurious blank context line on the last file.
    if lines.last() == Some(&"") {
        lines.pop();
    }

    let mut files: Vec<PatchFile> = Vec::new();
    let mut hunk_ops: Vec<LineOp> = Vec::new();
    let mut hunk_header = String::new();

    fn end_hunk(files: &mut [PatchFile], hunk_ops: &mut Vec<LineOp>, hunk_header: &mut String) {
        if !hunk_ops.is_empty() {
            if let Some(f) = files.last_mut() {
                f.hunks.push(PatchHunk {
                    header: hunk_header.clone(),
                    lines: segmentize(group_runs(std::mem::take(hunk_ops))),
                });
            }
        }
        hunk_ops.clear();
        hunk_header.clear();
    }

    for raw in lines {
        if raw.starts_with(BEGIN) || raw.starts_with(END) {
            continue;
        }

        if raw.starts_with(ADD) || raw.starts_with(UPDATE) || raw.starts_with(DELETE) {
            end_hunk(&mut files, &mut hunk_ops, &mut hunk_header);
            let (op, prefix) = if raw.starts_with(ADD) {
                (PatchFileOp::Add, ADD)
            } else if raw.starts_with(UPDATE) {
                (PatchFileOp::Update, UPDATE)
            } else {
                (PatchFileOp::Delete, DELETE)
            };
            files.push(PatchFile {
                path: raw[prefix.len()..].trim().to_string(),
                op,
                move_path: None,
                hunks: Vec::new(),
            });
            continue;
        }

        if let Some(dest) = raw.strip_prefix(MOVE) {
            if let Some(f) = files.last_mut() {
                f.move_path = Some(dest.trim().to_string());
            }
            continue;
        }

        if let Some(header) = raw.strip_prefix("@@") {
            end_hunk(&mut files, &mut hunk_ops, &mut hunk_header);
            hunk_header = header.trim().to_string();
            continue;
        }

        if files.is_empty() {
            continue; // stray line outside any file section
        }

        if let Some(text) = raw.strip_prefix('+') {
            hunk_ops.push(LineOp {
                kind: DiffLineKind::Added,
                text: text.to_string(),
            });
        } else if let Some(text) = raw.strip_prefix('-') {
            hunk_ops.push(LineOp {
                kind: DiffLineKind::Removed,
                text: text.to_string(),
            });
        } else if let Some(text) = raw.strip_prefix(' ') {
            hunk_ops.push(LineOp {
                kind: DiffLineKind::Context,
                text: text.to_string(),
            });
        } else {
            hunk_ops.push(LineOp {
                kind: DiffLineKind::Context,
                text: raw.to_string(),
            }); // blank/untagged context line
        }
    }
    end_hunk(&mut files, &mut hunk_ops, &mut hunk_header);

    if files.is_empty() {
        None
    } else {
        Some(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_non_patch_text() {
        assert!(parse_apply_patch("just some output text").is_none());
        assert!(parse_apply_patch("").is_none());
    }

    #[test]
    fn parses_an_update_with_context_removed_and_added_lines() {
        let patch = [
            "*** Begin Patch",
            "*** Update File: src/main.rs",
            "@@",
            " fn main() {",
            "-    println!(\"old\");",
            "+    println!(\"new\");",
            " }",
            "*** End Patch",
        ]
        .join("\n");

        let files = parse_apply_patch(&patch).unwrap();
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.op, PatchFileOp::Update);
        assert_eq!(file.path, "src/main.rs");
        assert_eq!(file.move_path, None);
        assert_eq!(file.hunks.len(), 1);

        let lines = &file.hunks[0].lines;
        let kinds: Vec<_> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::Context,
            ]
        );
        let line0: String = lines[0].segments.iter().map(|s| s.text.clone()).collect();
        assert_eq!(line0, "fn main() {");
        assert!(lines[1].segments.iter().any(|s| s.changed));
        assert!(lines[2].segments.iter().any(|s| s.changed));
        let removed: String = lines[1].segments.iter().map(|s| s.text.clone()).collect();
        assert_eq!(removed, "    println!(\"old\");");
        let added: String = lines[2].segments.iter().map(|s| s.text.clone()).collect();
        assert_eq!(added, "    println!(\"new\");");
    }

    #[test]
    fn parses_an_added_file_with_all_added_lines() {
        let patch = [
            "*** Begin Patch",
            "*** Add File: docs/README.md",
            "+# Title",
            "+",
            "+body",
            "*** End Patch",
        ]
        .join("\n");

        let files = parse_apply_patch(&patch).unwrap();
        assert_eq!(files[0].op, PatchFileOp::Add);
        assert_eq!(files[0].path, "docs/README.md");
        let lines = &files[0].hunks[0].lines;
        let kinds: Vec<_> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Added,
                DiffLineKind::Added,
                DiffLineKind::Added
            ]
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.segments.iter().map(|s| s.text.clone()).collect())
            .collect();
        assert_eq!(
            texts,
            vec!["# Title".to_string(), String::new(), "body".to_string()]
        );
    }

    #[test]
    fn parses_a_deleted_file_header_with_no_body() {
        let patch = [
            "*** Begin Patch",
            "*** Delete File: old/file.txt",
            "*** End Patch",
        ]
        .join("\n");
        let files = parse_apply_patch(&patch).unwrap();
        assert_eq!(files[0].op, PatchFileOp::Delete);
        assert_eq!(files[0].path, "old/file.txt");
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn captures_a_move_rename_target() {
        let patch = [
            "*** Begin Patch",
            "*** Update File: a/old.ts",
            "*** Move to: a/new.ts",
            "@@",
            "-const x = 1;",
            "+const x = 2;",
            "*** End Patch",
        ]
        .join("\n");
        let files = parse_apply_patch(&patch).unwrap();
        assert_eq!(files[0].move_path, Some("a/new.ts".to_string()));
    }

    #[test]
    fn splits_multiple_files_and_multiple_hunks() {
        let patch = [
            "*** Begin Patch",
            "*** Update File: a.ts",
            "@@ first hunk",
            "-a",
            "+b",
            "@@ second hunk",
            " keep",
            "+added",
            "*** Update File: b.ts",
            "@@",
            "-gone",
            "*** End Patch",
        ]
        .join("\n");
        let files = parse_apply_patch(&patch).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.ts");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].header, "first hunk");
        assert_eq!(files[0].hunks[1].header, "second hunk");
        assert_eq!(files[1].path, "b.ts");
        let kinds: Vec<_> = files[1].hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![DiffLineKind::Removed]);
    }

    #[test]
    fn does_not_emit_a_spurious_blank_line_from_a_trailing_newline() {
        let patch = [
            "*** Begin Patch",
            "*** Add File: f.txt",
            "+hi",
            "*** End Patch",
            "",
        ]
        .join("\n");
        let files = parse_apply_patch(&patch).unwrap();
        let kinds: Vec<_> = files[0].hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![DiffLineKind::Added]);
    }
}
