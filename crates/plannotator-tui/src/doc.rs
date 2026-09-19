//! Document model: the source text split into top-level Markdown blocks with byte ranges.
//!
//! The parser is `pulldown-cmark`; this module only walks its event stream at depth zero
//! and records where each block starts and ends. Everything downstream (rendering,
//! anchoring, hit-testing) is keyed by block index and byte range. Nothing here knows what
//! a heading or a list *is*.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Heading,
    Paragraph,
    List,
    CodeBlock,
    BlockQuote,
    Table,
    Rule,
    Html,
    /// YAML front matter; parsed for correct boundaries but never shown.
    Metadata,
    /// A unified-diff file header (`diff --git …` up to the first hunk).
    DiffFileHeader,
    /// One hunk of a unified diff: the `@@ … @@` line plus its body lines.
    DiffHunk,
    Other,
}

impl BlockKind {
    /// Code and tables keep their columns; everything else word-wraps. Diff lines are
    /// exact text: a wrapped `-` line would read as two removed lines.
    pub(crate) fn preserves_columns(self) -> bool {
        matches!(
            self,
            BlockKind::CodeBlock | BlockKind::Table | BlockKind::DiffFileHeader | BlockKind::DiffHunk
        )
    }

    /// Rendered by the diff renderer, not by the markdown renderer.
    pub(crate) fn is_diff(self) -> bool {
        matches!(self, BlockKind::DiffFileHeader | BlockKind::DiffHunk)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Block {
    pub(crate) range: Range<usize>,
    pub(crate) kind: BlockKind,
}

#[derive(Debug)]
pub(crate) struct Document {
    pub(crate) source: String,
    pub(crate) blocks: Vec<Block>,
}

/// The option set `tui-markdown` enables, so block boundaries agree with its renderer.
fn parse_options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_MATH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_GFM
        | Options::ENABLE_TABLES
}

impl Document {
    pub(crate) fn parse(source: String) -> Self {
        let blocks = split_blocks(&source);
        Self { source, blocks }
    }

    /// Split a unified diff into file-header and hunk blocks. One block per hunk, so a
    /// comment anchors to a whole change; everything between `diff --git` (or `---`) and
    /// the first hunk is that file's header. Leading lines before the first header — a
    /// `git format-patch` subject or mail headers — join the first header block.
    pub(crate) fn parse_diff(source: String) -> Self {
        let mut blocks = split_diff_blocks(&source);
        // Trailing newlines are not part of a block's text, matching `split_blocks`:
        // quotes stay stable across files that differ only in final-newline conventions.
        for block in &mut blocks {
            let text = &source[block.range.clone()];
            let trimmed = text.trim_end_matches(['\n', '\r']);
            block.range.end = block.range.start + trimmed.len();
        }
        blocks.retain(|b| !b.range.is_empty());
        Self { source, blocks }
    }

    /// Source text of block `index`. Empty for an out-of-range index.
    pub(crate) fn block_text(&self, index: usize) -> &str {
        self.blocks.get(index).map_or("", |b| &self.source[b.range.clone()])
    }

    /// The block whose range contains `offset`.
    pub(crate) fn block_containing(&self, offset: usize) -> Option<usize> {
        self.blocks.iter().position(|b| b.range.contains(&offset))
    }
}

fn kind_of(tag: &Tag<'_>) -> BlockKind {
    match tag {
        Tag::Heading { .. } => BlockKind::Heading,
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::List(_) => BlockKind::List,
        Tag::CodeBlock(_) => BlockKind::CodeBlock,
        Tag::BlockQuote(_) => BlockKind::BlockQuote,
        Tag::Table(_) => BlockKind::Table,
        Tag::HtmlBlock => BlockKind::Html,
        Tag::MetadataBlock(_) => BlockKind::Metadata,
        _ => BlockKind::Other,
    }
}

/// Walk the event stream and cut the source at depth-zero block boundaries.
fn split_blocks(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<(usize, BlockKind)> = None;

    for (event, range) in Parser::new_ext(source, parse_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    open = Some((range.start, kind_of(&tag)));
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some((start, kind)) = open.take()
                {
                    blocks.push(Block { range: start..range.end, kind });
                }
            }
            Event::Rule if depth == 0 => blocks.push(Block { range, kind: BlockKind::Rule }),
            // Any other depth-zero leaf (rare: stray html/text) becomes its own block.
            _ if depth == 0 => blocks.push(Block { range, kind: BlockKind::Other }),
            _ => {}
        }
    }

    // Trailing newlines are not part of a block's text: quotes stay stable across files
    // that differ only in final-newline conventions.
    for block in &mut blocks {
        let text = &source[block.range.clone()];
        let trimmed = text.trim_end_matches(['\n', '\r']);
        block.range.end = block.range.start + trimmed.len();
    }
    blocks.retain(|b| !b.range.is_empty() && b.kind != BlockKind::Metadata);
    blocks
}

/// Cut a unified diff into blocks by walking lines with their byte offsets. Inside a
/// hunk, old/new line counts from the `@@` header say how many `-`/` `/`+` body lines
/// still belong to it — the same rule git uses — so body text that *looks* like a header
/// (a removed line reading `--- x`) cannot split a hunk. A file section starts at
/// `diff --git` / `Index:`, or at `--- ` when no header is open; lines before the first
/// recognized header — a `git format-patch` subject or mail headers — join the first
/// header block.
fn split_diff_blocks(source: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    // split('\n') over `lines()`: every element costs exactly its length plus one byte,
    // so byte offsets stay exact even with CRLF input.
    let lines: Vec<&str> = source.split('\n').collect();
    let mut open: Option<(usize, BlockKind)> = None;
    let mut cursor = 0usize;
    // Body lines still owed to the open hunk: (old, new).
    let mut hunk: Option<(usize, usize)> = None;

    for (i, line) in lines.iter().enumerate() {
        let start = cursor;
        cursor += line.len() + 1;
        if let Some((old, new)) = hunk
            && (old > 0 || new > 0)
        {
            hunk = Some(match line.chars().next() {
                Some('-') => (old.saturating_sub(1), new),
                Some('+') => (old, new.saturating_sub(1)),
                // "\ No newline at end of file" belongs to the previous line.
                Some('\\') => (old, new),
                _ => (old.saturating_sub(1), new.saturating_sub(1)), // context ' '
            });
            continue;
        }
        let file_start = line.starts_with("diff --git ")
            || line.starts_with("Index: ")
            || (line.starts_with("--- ")
                && !matches!(open, Some((_, BlockKind::DiffFileHeader)))
                && lines.get(i + 1).is_some_and(|next| next.starts_with("+++ ")));
        if let Some((old, new)) = hunk_header(line) {
            if let Some((from, previous)) = open.take() {
                blocks.push(Block { range: from..start, kind: previous });
            }
            open = Some((start, BlockKind::DiffHunk));
            hunk = Some((old, new));
        } else if file_start {
            if let Some((from, previous)) = open.take() {
                blocks.push(Block { range: from..start, kind: previous });
            }
            open = Some((start, BlockKind::DiffFileHeader));
        } else if open.is_none() {
            open = Some((start, BlockKind::DiffFileHeader));
        }
    }
    if let Some((from, kind)) = open.take() {
        blocks.push(Block { range: from..source.len(), kind });
    }
    blocks
}

/// Old and new line counts from a `@@ -a[,b] +c[,d] @@` header. `None` for any other
/// line.
fn hunk_header(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("@@") {
        return None;
    }
    let old = parts.next()?;
    let new = parts.next()?;
    let count = |spec: &str| -> Option<usize> {
        let body = spec.strip_prefix(['-', '+'])?;
        match body.split_once(',') {
            Some((_, n)) => n.parse().ok(),
            None => Some(1),
        }
    };
    Some((count(old)?, count(new)?))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn splits_top_level_blocks_with_ranges() {
        let src = "# Title\n\nPara one\nstill one.\n\n- a\n- b\n\n```rs\nfn x() {}\n```\n\n---\n";
        let doc = Document::parse(src.to_owned());
        let kinds: Vec<_> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading,
                BlockKind::Paragraph,
                BlockKind::List,
                BlockKind::CodeBlock,
                BlockKind::Rule
            ]
        );
        assert_eq!(doc.block_text(0), "# Title");
        assert_eq!(doc.block_text(1), "Para one\nstill one.");
        assert_eq!(doc.block_text(3), "```rs\nfn x() {}\n```");
    }

    #[test]
    fn front_matter_is_dropped_and_nested_lists_stay_one_block() {
        let doc = Document::parse("---\ntitle: X\n---\n\n- a\n  - nested\n- b\n".to_owned());
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.blocks.first().map(|b| b.kind), Some(BlockKind::List));
    }

    const GIT_PATCH: &str = "diff --git a/src/app.rs b/src/app.rs\nindex 111..222 100644\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,3 +1,4 @@ fn main() {\n context\n-removed\n+added\n+more\n context\n@@ -10,3 +11,4 @@ next\n keep\n+tail\n done\ndiff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old one\n+new one\n";

    #[test]
    fn splits_a_git_patch_into_header_and_hunk_blocks() {
        let doc = Document::parse_diff(GIT_PATCH.to_owned());
        let kinds: Vec<_> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::DiffFileHeader,
                BlockKind::DiffHunk,
                BlockKind::DiffHunk,
                BlockKind::DiffFileHeader,
                BlockKind::DiffHunk
            ]
        );
        assert!(doc.block_text(0).starts_with("diff --git a/src/app.rs"));
        assert!(doc.block_text(0).contains("+++ b/src/app.rs"));
        assert!(doc.block_text(1).starts_with("@@ -1,3 +1,4 @@"));
        assert!(doc.block_text(1).contains("+added"));
        assert!(doc.block_text(2).starts_with("@@ -10,3 +11,4 @@"));
        assert!(doc.block_text(4).contains("+new one"));
    }

    #[test]
    fn diff_block_ranges_index_the_patch_text() {
        let doc = Document::parse_diff(GIT_PATCH.to_owned());
        let hunk = doc.blocks.get(1).expect("hunk");
        assert_eq!(&doc.source[hunk.range.clone()], doc.block_text(1));
    }

    #[test]
    fn a_plain_unified_diff_without_git_headers_parses() {
        let doc = Document::parse_diff("--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-a\n+b\n".to_owned());
        let kinds: Vec<_> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BlockKind::DiffFileHeader, BlockKind::DiffHunk]);
        assert!(doc.block_text(1).contains("+b"));
    }

    #[test]
    fn a_binary_marker_stays_in_its_header_block() {
        let patch = "diff --git a/logo.png b/logo.png\nindex 111..222 100644\nBinary files a/logo.png and b/logo.png differ\n";
        let doc = Document::parse_diff(patch.to_owned());
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.blocks.first().map(|b| b.kind), Some(BlockKind::DiffFileHeader));
        assert!(doc.block_text(0).contains("Binary files"));
    }

    #[test]
    fn format_patch_subject_lines_join_the_first_header() {
        let patch = "From 1234 Mon Sep 17 00:00:00 2001\nSubject: [PATCH] ship it\n\ndiff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
        let doc = Document::parse_diff(patch.to_owned());
        let kinds: Vec<_> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BlockKind::DiffFileHeader, BlockKind::DiffFileHeader, BlockKind::DiffHunk]);
        assert!(doc.block_text(0).starts_with("From 1234"));
        assert!(doc.block_text(1).starts_with("diff --git a/f"));
        assert!(doc.block_text(2).starts_with("@@ -1 +1 @@"));
    }

    #[test]
    fn hunk_bodies_consume_their_counted_lines_even_when_they_look_like_headers() {
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n--- looks like a header\n+++ and like an added one\n";
        let doc = Document::parse_diff(patch.to_owned());
        let kinds: Vec<_> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BlockKind::DiffFileHeader, BlockKind::DiffHunk]);
        assert!(doc.block_text(1).contains("--- looks like a header"));
    }
}
