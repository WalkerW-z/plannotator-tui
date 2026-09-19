//! What changed since the version the annotations were first written against.
//!
//! A file review captures its content the first time an annotation is saved
//! (`baseline.md` beside the record). When the file later differs — an agent applied
//! the feedback — the review shows the diff: gutter marks on changed rows, a `+N −M`
//! footer chip, and three verbs: accept (`a`), revert (`D`), inspect (`i`).

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use similar::{DiffOp, TextDiff};

/// Sidecar next to `annotations.json`: the content the reviewed round started from.
pub(crate) fn baseline_path(record: &Path) -> PathBuf {
    record.with_file_name("baseline.md")
}

pub(crate) fn read(record: &Path) -> Option<String> {
    std::fs::read_to_string(baseline_path(record)).ok()
}

pub(crate) fn write(record: &Path, content: &str) -> Result<()> {
    let path = baseline_path(record);
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))
}

/// The diff between the reviewed baseline and the current file, as byte ranges in the
/// current source so every mark lands through the existing row→offset map.
#[derive(Debug)]
pub(crate) struct Overlay {
    /// The content the annotations were first written against. Revert writes it back.
    pub(crate) baseline: String,
    /// Current-source ranges covering added (or rewritten) lines.
    pub(crate) added: Vec<Range<usize>>,
    /// Current-source ranges marking where baseline lines were removed. Each range
    /// covers the line(s) now standing where baseline content used to be.
    pub(crate) removed: Vec<Range<usize>>,
    pub(crate) added_lines: usize,
    pub(crate) removed_lines: usize,
}

impl Overlay {
    /// `None` when there is no baseline or the file matches it.
    pub(crate) fn between(baseline: &str, current: &str) -> Option<Self> {
        if baseline == current {
            return None;
        }
        let starts = line_starts(current);
        let diff = TextDiff::from_lines(baseline, current);
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut added_lines = 0usize;
        let mut removed_lines = 0usize;
        for op in diff.ops() {
            match *op {
                DiffOp::Equal { .. } => {}
                DiffOp::Delete { old_len, new_index, .. } => {
                    removed_lines += old_len;
                    removed.push(current_range(&starts, new_index, new_index, current.len()));
                }
                DiffOp::Insert { new_index, new_len, .. } => {
                    added_lines += new_len;
                    added.push(current_range(&starts, new_index, new_index + new_len, current.len()));
                }
                DiffOp::Replace { old_len, new_index, new_len, .. } => {
                    removed_lines += old_len;
                    added_lines += new_len;
                    removed.push(current_range(&starts, new_index, new_index, current.len()));
                    added.push(current_range(&starts, new_index, new_index + new_len, current.len()));
                }
            }
        }
        Some(Self { baseline: baseline.to_owned(), added, removed, added_lines, removed_lines })
    }

    /// Whether `offset` sits inside added content.
    pub(crate) fn is_added(&self, offset: usize) -> bool {
        self.added.iter().any(|r| r.contains(&offset))
    }

    /// Whether `offset` sits on a line standing where baseline content was removed.
    pub(crate) fn is_removed(&self, offset: usize) -> bool {
        self.removed.iter().any(|r| r.contains(&offset))
    }
}

/// Byte range of current-source lines `from..to` (line indices into the `\n` split).
/// A pure deletion (`to == from`) has no current lines of its own: mark the line that
/// now stands at that position, so the row the user sees carries the red mark. A
/// deletion past the last line (at EOF) has nothing on screen to mark — empty range.
fn current_range(starts: &[usize], from: usize, to: usize, source_len: usize) -> Range<usize> {
    let start = starts.get(from).copied().unwrap_or(source_len);
    if to == from {
        let next = starts.get(from + 1).copied().unwrap_or(source_len);
        return if next > start { start..next } else { start..start };
    }
    let end = starts.get(to).copied().unwrap_or(source_len);
    start..end.max(start)
}

/// Byte offset of every line start (`split('\n')` indexing, so offsets are exact).
fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn identical_content_has_no_overlay() {
        assert!(Overlay::between("same\n", "same\n").is_none());
    }

    #[test]
    fn added_lines_map_to_their_current_byte_ranges() {
        let baseline = "one\ntwo\n";
        let current = "one\nnew\ntwo\n";
        let overlay = Overlay::between(baseline, current).expect("differs");
        assert_eq!(overlay.added_lines, 1);
        assert_eq!(overlay.removed_lines, 0);
        let at = current.find("new").expect("present");
        assert!(overlay.is_added(at), "'new' at {at} must be marked added");
        assert!(overlay.is_added(at + 2));
        assert!(!overlay.is_added(current.find("two").expect("present")));
    }

    #[test]
    fn removed_lines_mark_where_they_stood() {
        let baseline = "one\ndead\ntwo\n";
        let current = "one\ntwo\n";
        let overlay = Overlay::between(baseline, current).expect("differs");
        assert_eq!(overlay.removed_lines, 1);
        assert_eq!(overlay.added_lines, 0);
        let at = current.find("two").expect("present");
        assert!(overlay.is_removed(at), "the line now at the removal point is marked");
        assert!(!overlay.is_removed(current.find("one").expect("present")));
    }

    #[test]
    fn rewrites_count_as_both_and_the_whole_run_is_marked() {
        let baseline = "a\nold line\nkeep\n";
        let current = "a\nnew one\nnew two\nkeep\n";
        let overlay = Overlay::between(baseline, current).expect("differs");
        assert_eq!(overlay.added_lines, 2);
        assert_eq!(overlay.removed_lines, 1);
        let start = current.find("new one").expect("present");
        let end = current.find("\nkeep").expect("present");
        assert!(overlay.is_added(start + 1));
        assert!(overlay.is_added(end - 1));
        assert!(!overlay.is_added(current.find("keep").expect("present")));
    }

    #[test]
    fn ranges_stay_inside_the_source_at_eof() {
        let overlay = Overlay::between("a\nb\n", "a\nb\nc").expect("differs");
        let last = overlay.added.last().expect("added c");
        assert!(last.end <= 5, "range {last:?} must not run past 'a\\nb\\nc'");
    }
}
