//! What changed since the version the annotations were first written against.
//!
//! A file review captures its content the first time an annotation is saved
//! (`baseline.md` beside the record). When the file later differs — an agent applied
//! the feedback — the review shows the diff: gutter marks on changed rows, a `+N −M`
//! footer chip, and the review verbs: accept (`a`), un-accept (`U`), revert (`D`),
//! inspect (`i`).
//!
//! Two sidecars make the verbs reversible. `baseline.md` is the reference the overlay
//! marks against; accepting a region folds the file's content into it. `reviewed.md` is
//! the version the round started from and is never touched by a region accept, so
//! un-accept can put the reviewed text back.

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use similar::{ChangeTag, DiffOp, DiffTag, TextDiff};

/// Sidecar next to `annotations.json`: the reference the overlay marks against. Region
/// accepts move it forward; `reviewed.md` keeps the version the round started from.
pub(crate) fn baseline_path(record: &Path) -> PathBuf {
    record.with_file_name("baseline.md")
}

/// Sidecar next to `annotations.json`: the version the reviewed round started from. It
/// is written with `baseline.md` and never moved by a region accept, so an accepted
/// region can be un-accepted back to it.
pub(crate) fn reviewed_path(record: &Path) -> PathBuf {
    record.with_file_name("reviewed.md")
}

pub(crate) fn read(record: &Path) -> Option<String> {
    std::fs::read_to_string(baseline_path(record)).ok()
}

pub(crate) fn write(record: &Path, content: &str) -> Result<()> {
    write_to(&baseline_path(record), content)
}

pub(crate) fn read_reviewed(record: &Path) -> Option<String> {
    std::fs::read_to_string(reviewed_path(record)).ok()
}

pub(crate) fn write_reviewed(record: &Path, content: &str) -> Result<()> {
    write_to(&reviewed_path(record), content)
}

fn write_to(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
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
    /// A unified diff covering the whole file — every baseline and current line, with
    /// context lines prefixed ` `, removed `-`, added `+`. Rendered as the changes
    /// view: the whole file with the diff inline. Built in the same pass as the
    /// ranges, so opening a changed file diffs once, not twice.
    pub(crate) full_diff: String,
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
        Some(Self {
            baseline: baseline.to_owned(),
            added,
            removed,
            added_lines,
            removed_lines,
            full_diff: full_context_diff(&diff),
        })
    }

    /// Whether `offset` sits inside added content.
    pub(crate) fn is_added(&self, offset: usize) -> bool {
        self.added.iter().any(|r| r.contains(&offset))
    }

    /// Whether `offset` sits on a line standing where baseline content was removed.
    pub(crate) fn is_removed(&self, offset: usize) -> bool {
        self.removed.iter().any(|r| r.contains(&offset))
    }

    /// Whether a block's byte range covers any change, added or removed.
    pub(crate) fn touches(&self, range: &Range<usize>) -> bool {
        let hit = |r: &Range<usize>| r.start < range.end && range.start < r.end;
        self.added.iter().any(hit) || self.removed.iter().any(hit)
    }
}

/// The whole-file unified diff for the changes view: every line of both versions in
/// reading order — context prefixed ` `, removed `-`, added `+` — under one hunk
/// header whose counts cover the file. Built from the same `TextDiff` the ranges
/// came from, so opening a changed file diffs once.
fn full_context_diff(diff: &TextDiff<'_, '_, str>) -> String {
    let mut body = String::new();
    let (mut old_total, mut new_total) = (0usize, 0usize);
    for change in diff.iter_all_changes() {
        match change.tag() {
            // Context lines belong to both sides; the hunk counts say so.
            ChangeTag::Equal => {
                old_total += 1;
                new_total += 1;
            }
            ChangeTag::Delete => old_total += 1,
            ChangeTag::Insert => new_total += 1,
        }
        let prefix = match change.tag() {
            ChangeTag::Equal => ' ',
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
        };
        body.push(prefix);
        body.push_str(&change.to_string_lossy());
        if change.missing_newline() {
            body.push('\n');
            body.push_str("\\ No newline at end of file\n");
        }
    }
    format!("@@ -1,{old_total} +1,{new_total} @@\n{body}")
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

/// Line span (indices into `split('\n')`) touched by `bytes`. Used to turn a block's
/// byte range into the lines a region verb operates on. An empty range at `p` covers the
/// line holding `p` (for a mid-file `p`) or nothing at EOF.
pub(crate) fn line_span(text: &str, bytes: &Range<usize>) -> Range<usize> {
    let end = bytes.end.min(text.len());
    let start = bytes.start.min(end);
    let first = line_of(text, start);
    if start == end {
        return first..first;
    }
    first..line_of(text, end - 1) + 1
}

/// Index of the line holding byte `pos`.
fn line_of(text: &str, pos: usize) -> usize {
    text[..pos.min(text.len())].matches('\n').count()
}

/// Fold `current`'s content for the block at `block` into `baseline`: the changed lines
/// the block covers become the current file's, every other change keeps the baseline's.
/// Returns `baseline` unchanged when the block covers no change.
pub(crate) fn accept_region(baseline: &str, current: &str, block: &Range<usize>) -> String {
    transform(baseline, current, &line_span(current, block), false)
}

/// Rewrite `current` with the baseline's content for the block at `block`: the changed
/// lines the block covers go back to the reviewed version, every other change stays.
pub(crate) fn revert_region(baseline: &str, current: &str, block: &Range<usize>) -> String {
    transform(baseline, current, &line_span(current, block), true)
}

/// Put the reviewed text back into `baseline` for the block at `block`, so an accepted
/// region shows as changed again. `baseline` maps the block's line span first (an
/// accepted region is `Equal` between the two), then the reviewed side is taken there.
/// Returns `baseline` unchanged when nothing there was accepted.
pub(crate) fn restore_region(reviewed: &str, baseline: &str, current: &str, block: &Range<usize>) -> String {
    let diff = TextDiff::from_lines(baseline, current);
    let span = line_span(current, block);
    let Some(span) = map_new_to_old(&diff, &span) else { return baseline.to_owned() };
    transform(reviewed, baseline, &span, true)
}

/// The `baseline` line span matching `span` in `current`, when the whole span sits in an
/// `Equal` run. `None` when it overlaps a change — that region was never accepted.
fn map_new_to_old(diff: &TextDiff<'_, '_, str>, span: &Range<usize>) -> Option<Range<usize>> {
    for op in diff.ops() {
        let new = op.new_range();
        if new.is_empty() || new.start >= span.end || span.start >= new.end {
            continue;
        }
        if op.tag() != DiffTag::Equal {
            return None;
        }
        let old = op.old_range();
        let start = old.start + span.start.saturating_sub(new.start);
        let end = old.start + span.end.min(new.end).saturating_sub(new.start);
        return Some(start..end.max(start));
    }
    None
}

/// Rebuild the older text by walking the diff: `Equal` lines are copied, and a changed
/// op takes the older side when it sits inside `span` and `take_old_inside` is set (or
/// when it sits outside and the flag is clear). `span` indexes lines of the newer text.
fn transform(older: &str, newer: &str, span: &Range<usize>, take_old_inside: bool) -> String {
    let diff = TextDiff::from_lines(older, newer);
    let mut out = String::with_capacity(older.len());
    for op in diff.ops() {
        let keep_old = match op.tag() {
            DiffTag::Equal => true,
            _ => intersects(&op.new_range(), span) == take_old_inside,
        };
        for change in diff.iter_changes(op) {
            let include = match change.tag() {
                ChangeTag::Equal => true,
                ChangeTag::Delete => keep_old,
                ChangeTag::Insert => !keep_old,
            };
            if include {
                out.push_str(change.value());
            }
        }
    }
    out
}

/// Whether a changed op's `new` range falls inside `span`. A pure deletion has an empty
/// `new` range; its position counts as inside when it sits on a line the span covers.
fn intersects(new: &Range<usize>, span: &Range<usize>) -> bool {
    if new.is_empty() {
        new.start >= span.start && new.start < span.end
    } else {
        new.start < span.end && span.start < new.end
    }
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
    #[test]
    fn line_span_covers_the_lines_a_block_touches() {
        let text = "# Plan\n\none changed\n\ntwo\n";
        let start = text.find("one changed").expect("present");
        let block = start..start + "one changed".len();
        assert_eq!(line_span(text, &block), 2..3);
        let two = text.find("two").expect("present");
        assert_eq!(line_span(text, &(two..two + 3)), 4..5);
        assert_eq!(line_span(text, &(0..0)), 0..0);
    }

    #[test]
    fn accepting_a_block_folds_only_that_block() {
        let baseline = "# Plan\n\none\n\ntwo\n\nthree\n";
        let current = "# Plan\n\none changed\n\ntwo rewritten\n\nthree\n";
        let block = current.find("one changed").expect("present");
        let accepted = accept_region(baseline, current, &(block..block + "one changed".len()));
        assert_eq!(accepted, "# Plan\n\none changed\n\ntwo\n\nthree\n", "only the hovered block moved");
        // The other change still shows against the folded baseline.
        let overlay = Overlay::between(&accepted, current).expect("two still differs");
        let at = current.find("two rewritten").expect("present");
        assert!(overlay.is_added(at));
        assert!(!overlay.is_added(current.find("one changed").expect("present")));
    }

    #[test]
    fn reverting_a_block_restores_only_that_block() {
        let baseline = "# Plan\n\none\n\ntwo\n\nthree\n";
        let current = "# Plan\n\none changed\n\ntwo rewritten\n\nthree\n";
        let block = current.find("two rewritten").expect("present");
        let reverted = revert_region(baseline, current, &(block..block + "two rewritten".len()));
        assert_eq!(
            reverted, "# Plan\n\none changed\n\ntwo\n\nthree\n",
            "the hovered block is back to the reviewed text"
        );
    }

    #[test]
    fn un_accepting_a_block_marks_it_changed_again() {
        let reviewed = "# Plan\n\none\n\ntwo\n\nthree\n";
        let current = "# Plan\n\none changed\n\ntwo\n\nthree\n";
        let block = current.find("one changed").expect("present");
        let block = block..block + "one changed".len();
        let accepted = accept_region(reviewed, current, &block);
        assert_eq!(accepted, current, "the whole change was the hovered block");
        let restored = restore_region(reviewed, &accepted, current, &block);
        assert_eq!(restored, reviewed, "the reviewed text is back in the baseline");
        assert!(Overlay::between(&restored, current).is_some(), "the change shows again");
    }

    #[test]
    fn un_accepting_an_unchanged_block_changes_nothing() {
        let reviewed = "# Plan\n\none\n\ntwo\n";
        let current = "# Plan\n\none changed\n\ntwo\n";
        let changed = current.find("one changed").expect("present");
        let accepted = accept_region(reviewed, current, &(changed..changed + "one changed".len()));
        let two = current.find("two").expect("present");
        let restored = restore_region(reviewed, &accepted, current, &(two..two + 3));
        assert_eq!(restored, accepted, "a block with no change cannot be un-accepted");
    }

    #[test]
    fn full_diff_covers_the_whole_file_with_inline_changes() {
        let baseline = "# Title\n\nfirst\n\nkeep\n";
        let current = "# Title\n\nfirst (expanded)\n\nkeep\n\nadded tail\n";
        let overlay = Overlay::between(baseline, current).expect("differs");
        let lines: Vec<&str> = overlay.full_diff.lines().collect();
        assert_eq!(
            lines.first().copied().expect("hunk header"),
            "@@ -1,5 +1,7 @@",
            "one hunk covers the file"
        );
        assert!(lines.contains(&"-first"), "removed line present");
        assert!(lines.contains(&"+first (expanded)"), "added line present");
        assert!(lines.contains(&"+added tail"), "added tail present");
        assert!(lines.contains(&" # Title"), "context line present");
        assert!(lines.contains(&" keep"), "unchanged tail present");
    }
}
