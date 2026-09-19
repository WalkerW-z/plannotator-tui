//! What the app opens: a document source, not necessarily a file.
//!
//! A file on disk, a Workspaces document, and an agent's last reply are all sources. The
//! `transient` flag is the only behavioural switch: transient sources get no sidecar, no
//! history, no drafts. Provenance is opaque to the schema and meaningful to the delivery
//! seam (where feedback goes back to).

use crate::version::blob_sha;

/// Where a document came from. The app never interprets these beyond display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// A file on this machine.
    File { path: std::path::PathBuf },
    /// A Workspaces document.
    Workspace { workspace_id: String, document_id: String },
    /// An agent's message, handed in by a host integration.
    AgentMessage { host: String, session: Option<String>, message_id: Option<String> },
    /// Standard input or another one-off feed.
    Stdin,
}

/// How the content parses. The app picks the parser from this, not from the
/// provenance or the file name; a source records what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// Markdown, block-split by the app's markdown parser.
    Markdown,
    /// A unified diff (git or plain). File headers and hunks render as blocks.
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSource {
    /// Raw markdown. Byte offsets in anchors index into this exact string.
    pub content: String,
    /// What to call it in the UI.
    pub name: String,
    /// True when nothing about this document should be persisted.
    pub transient: bool,
    pub provenance: Provenance,
    /// Git blob sha of `content`; the `version` every anchor made against it carries.
    pub version: String,
    /// How `content` parses.
    pub format: SourceFormat,
}

impl DocumentSource {
    pub fn new(content: String, name: impl Into<String>, transient: bool, provenance: Provenance) -> Self {
        let version = blob_sha(content.as_bytes());
        Self { content, name: name.into(), transient, provenance, version, format: SourceFormat::Markdown }
    }

    pub fn file(path: std::path::PathBuf, content: String) -> Self {
        let name =
            path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        Self::new(content, name, false, Provenance::File { path })
    }

    /// A unified diff as a transient document. Comments live for the session and the
    /// patch text is the anchor surface: a regenerated patch is a new review, never a
    /// stale continuation of the old one.
    pub fn diff(path: std::path::PathBuf, content: String) -> Self {
        let mut source = Self::file(path, content);
        source.transient = true;
        source.format = SourceFormat::Diff;
        source
    }
}
