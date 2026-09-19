//! Per-block rendering and the row map.
//!
//! Each block is rendered in isolation through `tui-markdown` (cached, width-independent),
//! aligned back to its source bytes, and wrapped to the current column width. Every screen
//! cell therefore knows its block and, where it shows real text, its source byte.

use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use tui_markdown::{Options, StyleSheet};

use crate::doc::{BlockKind, Document};
use crate::srcmap::{LineOffsets, align};
use crate::wrap::{Row, clip_line, wrap_line};

/// Rows of vertical space between blocks.
const BLOCK_GAP: usize = 1;

/// House style: no `#` markers, headings carry weight through bold/underline rather than
/// background color, so the palette stays available for selection and annotations.
#[derive(Debug, Clone)]
struct Styles;

impl StyleSheet for Styles {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            2 => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            _ => Style::new().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
        }
    }
    fn heading_marker(&self, _level: u8) -> &'static str {
        ""
    }
    fn code(&self) -> Style {
        Style::new().fg(Color::LightYellow)
    }
    fn link(&self) -> Style {
        Style::new().fg(Color::Blue).add_modifier(Modifier::UNDERLINED)
    }
    fn blockquote(&self) -> Style {
        Style::new().fg(Color::Green).add_modifier(Modifier::ITALIC)
    }
}

#[derive(Debug)]
pub(crate) struct RenderedBlock {
    /// Width-independent styled lines from the renderer (owned, cached).
    text: Text<'static>,
    /// Per line, per char: absolute source byte offset (cached with `text`).
    offsets: Vec<LineOffsets>,
    kind: BlockKind,
    /// Rows for the current width.
    pub(crate) rows: Vec<Row>,
    /// First screen row of this block in document coordinates.
    pub(crate) first_row: usize,
}

#[derive(Debug)]
pub(crate) struct DocLayout {
    pub(crate) width: usize,
    pub(crate) blocks: Vec<RenderedBlock>,
    pub(crate) total_rows: usize,
}

fn render_block(doc: &Document, index: usize) -> (Text<'static>, Vec<LineOffsets>) {
    let kind = doc.blocks.get(index).map_or(BlockKind::Other, |b| b.kind);
    if kind.is_diff() {
        return render_diff_block(doc, index, kind);
    }
    let source = doc.block_text(index);
    let text = own(tui_markdown::from_str_with_options(source, &Options::new(Styles)));
    let plain: Vec<String> = text.lines.iter().map(ToString::to_string).collect();
    let base = doc.blocks.get(index).map_or(0, |b| b.range.start);
    let offsets = align(&plain, source, base);
    (text, offsets)
}

/// House palette for diff lines: foreground color only, keeping backgrounds free for
/// selection and annotation highlighting.
fn diff_line_style(line: &str, kind: BlockKind) -> Style {
    match kind {
        BlockKind::DiffFileHeader => Style::new().fg(Color::DarkGray),
        _ => match line.chars().next() {
            Some('+') => Style::new().fg(Color::Green),
            Some('-') => Style::new().fg(Color::Red),
            Some('@') => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            Some('\\') => Style::new().fg(Color::DarkGray),
            _ => Style::new(),
        },
    }
}

/// Render a diff block: verbatim lines, one per source line, styled by prefix. The
/// offset map is the identity — every rendered character indexes its own source byte —
/// so selection and annotations land exactly where the eye puts them.
fn render_diff_block(doc: &Document, index: usize, kind: BlockKind) -> (Text<'static>, Vec<LineOffsets>) {
    let source = doc.block_text(index);
    let base = doc.blocks.get(index).map_or(0, |b| b.range.start);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut offsets: Vec<LineOffsets> = Vec::new();
    let mut line_start = 0usize;
    for raw in source.split('\n') {
        let style = diff_line_style(raw, kind);
        lines.push(Line::from(vec![Span::styled(raw.to_owned(), style)]));
        offsets.push(raw.char_indices().map(|(bi, _)| Some(base + line_start + bi)).collect());
        line_start += raw.len() + 1;
    }
    (Text::from(lines), offsets)
}

fn own(text: Text<'_>) -> Text<'static> {
    let lines = text
        .lines
        .into_iter()
        .map(|line| {
            let spans = line.spans.into_iter().map(|s| Span::styled(s.content.into_owned(), s.style));
            Line::from(spans.collect::<Vec<_>>()).style(line.style)
        })
        .collect::<Vec<_>>();
    Text::from(lines).style(text.style)
}

impl DocLayout {
    /// Render every block once (the expensive part) and lay out for `width`.
    pub(crate) fn build(doc: &Document, width: usize) -> Self {
        let blocks = doc
            .blocks
            .iter()
            .enumerate()
            .map(|(i, block)| {
                let (text, offsets) = render_block(doc, i);
                RenderedBlock { text, offsets, kind: block.kind, rows: Vec::new(), first_row: 0 }
            })
            .collect();
        let mut layout = Self { width: 0, blocks, total_rows: 0 };
        layout.reflow(width);
        layout
    }

    /// Re-wrap for a new width without re-rendering markdown.
    pub(crate) fn reflow(&mut self, width: usize) {
        let width = width.max(1);
        self.width = width;
        let mut row = 0usize;
        for block in &mut self.blocks {
            block.first_row = row;
            let lines = block.text.lines.iter().zip(&block.offsets);
            block.rows = if block.kind.preserves_columns() {
                lines.map(|(l, o)| clip_line(l, o, width)).collect()
            } else {
                lines.flat_map(|(l, o)| wrap_line(l, o, width)).collect()
            };
            row += block.rows.len() + BLOCK_GAP;
        }
        self.total_rows = row.saturating_sub(BLOCK_GAP);
    }

    /// Which block owns a document row (gap rows belong to nobody).
    pub(crate) fn block_at_row(&self, row: usize) -> Option<usize> {
        let idx = self.blocks.partition_point(|b| b.first_row <= row);
        let i = idx.checked_sub(1)?;
        let block = self.blocks.get(i)?;
        (row < block.first_row + block.rows.len()).then_some(i)
    }

    /// The row at document coordinate `row`, if it exists (None for gap rows).
    pub(crate) fn row(&self, row: usize) -> Option<&Row> {
        let block = self.blocks.get(self.block_at_row(row)?)?;
        block.rows.get(row - block.first_row)
    }

    /// First document row on which any cell falls inside `range`.
    pub(crate) fn first_row_in_range(&self, block: usize, range: &Range<usize>) -> Option<usize> {
        let b = self.blocks.get(block)?;
        b.rows
            .iter()
            .position(|r| r.cells.iter().any(|c| c.is_some_and(|o| range.contains(&o))))
            .map(|i| b.first_row + i)
    }

    /// The rendered text under a source range, in the web client's form: rendered
    /// characters whose source offset falls in `range`, blocks joined with no separator.
    ///
    /// The renderer maps a soft line break to nothing, so a break inside a wrapping block
    /// is recovered from the source: when consecutive rendered characters skip over
    /// whitespace in the source, one space is emitted (the DOM renders a soft break as a
    /// space). Code and tables keep their newlines.
    pub(crate) fn rendered_in_range(&self, source: &str, range: &Range<usize>) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            let break_char = if block.kind.preserves_columns() { '\n' } else { ' ' };
            let mut last_offset: Option<usize> = None;
            let chars = block.text.lines.iter().zip(&block.offsets).flat_map(|(line, offsets)| {
                line.spans.iter().flat_map(|s| s.content.chars()).zip(offsets.iter())
            });
            for (ch, offset) in chars {
                let Some(offset) = offset.filter(|o| range.contains(o)) else { continue };
                if let Some(prev) = last_offset
                    && let Some(skipped) = source.get(prev..offset)
                    && skipped.chars().any(char::is_whitespace)
                    && !out.ends_with(char::is_whitespace)
                {
                    out.push(break_char);
                }
                out.push(ch);
                last_offset = Some(offset + ch.len_utf8());
            }
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn rendered_text_strips_markup_and_joins_blocks_without_separator() {
        let doc = Document::parse("Ship the **login page**\nby Friday.\n\nNext para.\n".to_owned());
        let layout = DocLayout::build(&doc, 80);
        let whole = 0..doc.source.len();
        assert_eq!(layout.rendered_in_range(&doc.source, &whole), "Ship the login page by Friday.Next para.");
        let bold = doc.source.find("**login").expect("present");
        let bold_range = bold..bold + "**login page**".len();
        assert_eq!(layout.rendered_in_range(&doc.source, &bold_range), "login page");
    }

    #[test]
    fn diff_lines_render_verbatim_in_prefix_colors() {
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-old\n+new\n";
        let doc = Document::parse_diff(patch.to_owned());
        let layout = DocLayout::build(&doc, 80);
        let header = layout.blocks.first().expect("header");
        let hunk = layout.blocks.get(1).expect("hunk");
        assert_eq!(
            header.rows.iter().map(|r| r.line.to_string()).collect::<Vec<_>>(),
            ["diff --git a/f b/f", "--- a/f", "+++ b/f"]
        );
        assert!(header.rows.iter().all(|r| r.line.spans.iter().all(|s| s.style.fg == Some(Color::DarkGray))));
        assert_eq!(
            hunk.rows.iter().map(|r| r.line.to_string()).collect::<Vec<_>>(),
            ["@@ -1,2 +1,2 @@", "-old", "+new"]
        );
        let colors: Vec<_> =
            hunk.rows.iter().map(|r| r.line.spans.first().and_then(|s| s.style.fg)).collect();
        assert_eq!(colors, [Some(Color::Cyan), Some(Color::Red), Some(Color::Green)]);
    }

    #[test]
    fn diff_offsets_map_every_character_to_its_own_source_byte() {
        let patch = "@@ -1 +1 @@\n-old\n+new\n";
        let doc = Document::parse_diff(patch.to_owned());
        let layout = DocLayout::build(&doc, 80);
        let hunk = layout.blocks.first().expect("hunk");
        // Row 1 is "-old": its first rendered char is the '-' at its source position.
        let row = hunk.rows.get(1).expect("removed row");
        let dash = patch.find("-old").expect("present") as u32;
        assert_eq!(row.cells.first().copied().flatten().map(|b| b as u32), Some(dash));
        assert_eq!(row.cells.get(1).copied().flatten().map(|b| b as u32), Some(dash + 1));
    }

    #[test]
    fn diff_lines_clip_instead_of_wrap() {
        let long = "+".repeat(200);
        let patch = format!("@@ -1 +1 @@\n{long}\n");
        let doc = Document::parse_diff(patch);
        let layout = DocLayout::build(&doc, 80);
        let hunk = layout.blocks.first().expect("hunk");
        assert_eq!(hunk.rows.len(), 2); // header + one clipped body row
    }
}
