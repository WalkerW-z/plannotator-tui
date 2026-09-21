//! Application state. Input handling lives in `input`, drawing in `draw`; this module owns
//! the data they share and the operations that change it.

mod archive_view;
mod compose;
mod draw;
mod feedback;
mod header;
mod input;
mod menu;
mod pick;
mod review;
#[cfg(test)]
mod review_test_support;
mod selection;
mod send;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use plannotator_tui_schema::{DocumentSource, Kind, Provenance, SourceFormat};
use ratatui::layout::Rect;

use crate::delivery::Delivery;
use crate::doc::Document;
use crate::layout::DocLayout;
use crate::overlay;
use crate::store::{Location, Store};
use crate::tree::Tree;
use crate::workspace_paths;
use selection::Selection;
use send::SendState;

/// Width of the marker column left of the document.
pub(super) const GUTTER: u16 = 2;

/// Toolbar items in display order: (glyph, label, key, kind).
const TOOLBAR: [(&str, &str, char, Kind); 3] = [
    ("👍", "looks good", 'a', Kind::LooksGood),
    ("💬", "comment", 'c', Kind::Comment),
    ("✗", "delete", 'd', Kind::Delete),
];

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Browse,
    /// Typing a comment for the pending selection.
    Compose,
    /// Editing the body of an existing annotation (by id).
    Edit(String),
    /// Quit was asked for while feedback is unsent; the footer asks first.
    ConfirmQuit,
    /// Choosing which of the agent's recent messages to review.
    Pick,
    /// Restoring annotations from finished file reviews.
    Archive,
    /// The header's Review menu is open over a file or folder review.
    ReviewMenu,
}

/// Which pane keyboard input goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Tree,
    Document,
    Rail,
}

/// Screen geometry captured during the last draw, for hit-testing input.
#[derive(Debug, Default, Clone)]
struct Geometry {
    tree: Rect,
    doc: Rect,
    /// Toolbar rect and the column span of each item, in screen coordinates.
    toolbar: Option<(Rect, [Range<u16>; 3])>,
    /// Screen rects of the rail bubbles drawn last frame, with their annotation ids.
    bubbles: Vec<(Rect, String)>,
    /// The header's Send button; `None` when the header was too narrow for it.
    send_button: Option<Rect>,
    /// The header's Review button; only file and folder reviews draw it.
    review_button: Option<Rect>,
    undo_button: Option<Rect>,
    /// The Review menu drawn last frame and its rows, with their action index.
    menu: Option<Rect>,
    menu_rows: Vec<(Rect, usize)>,
    archive_rows: Vec<(Rect, usize)>,
    /// Picker rows drawn last frame, with their candidate index.
    pick_rows: Vec<(Rect, usize)>,
}

/// A finished selection waiting for an action.
#[derive(Debug, Clone)]
struct Pending {
    range: Range<usize>,
    /// Document (row, col) where the selection starts; anchors the toolbar and compose box.
    at: (usize, usize),
}

/// Everything about the open document; swapped wholesale when the tree switches files.
#[derive(Debug)]
struct Open {
    source: DocumentSource,
    doc: Document,
    layout: DocLayout,
    store: Store,
    /// Changes since the version the annotations were first written against; `None`
    /// for transient documents, fresh files, and files unchanged since review.
    overlay: Option<overlay::Overlay>,
}

impl Open {
    fn new(source: DocumentSource, width: usize, data_dir: &Path, project: &str) -> Result<Self> {
        let doc = match source.format {
            SourceFormat::Markdown => Document::parse(source.content.clone()),
            SourceFormat::Diff => Document::parse_diff(source.content.clone()),
        };
        let layout = DocLayout::build(&doc, width);
        let store = match (&source.provenance, source.transient) {
            (Provenance::File { path }, false) => {
                Store::load(&Location::for_file(data_dir, project, path), &doc)?
            }
            _ => Store::transient(),
        };
        let overlay = Self::diff_overlay(&source, &store);
        Ok(Self { source, doc, layout, store, overlay })
    }

    /// The reviewed version comes from the baseline sidecar written when the round's
    /// first annotation was saved. Transient documents (diffs, agent messages) have no
    /// baseline: their reviews are single-round by design.
    fn diff_overlay(source: &DocumentSource, store: &Store) -> Option<overlay::Overlay> {
        if source.transient {
            return None;
        }
        let record = store.location_record()?;
        let baseline = overlay::read(record)?;
        overlay::Overlay::between(&baseline, &source.content)
    }
}

use self::compose::Compose;

pub(crate) struct App {
    open: Open,
    /// The file review behind the changes view. `Some` only while the changes view is
    /// on screen; the md review inside is untouched, so swapping back restores it.
    changes_open: Option<Open>,
    /// `+added −removed` of the stashed review, for the footer chip while the changes
    /// view (whose own Open has no overlay) is showing.
    changes_counts: Option<(usize, usize)>,
    /// The file review's block and scroll while the changes view is on screen, so `i`
    /// returns to where the review was instead of the top of the file.
    file_selected: usize,
    file_scroll: usize,
    /// Where annotations are stored and how this folder is named there.
    data_dir: PathBuf,
    project: String,
    /// Present in folder mode.
    tree: Option<Tree>,
    tree_cursor: usize,
    /// First tree row drawn; follows `tree_cursor` so the selected row stays visible.
    tree_scroll: usize,
    /// `t` toggles; `None` means "automatic by width".
    tree_visible: Option<bool>,
    delivery: Box<dyn Delivery>,
    send_state: SendState,
    folder_counts: HashMap<PathBuf, feedback::ReviewCounts>,
    /// Annotated files the folder counts could not read, in `review_files` order.
    unreadable_files: Vec<PathBuf>,
    undo_archive: Vec<review::ArchivedBatch>,
    archive_items: Vec<review::ArchivedItem>,
    archive_cursor: usize,
    /// The highlighted row of the Review menu.
    menu_cursor: usize,
    focus: Focus,
    scroll: usize,
    selected: usize,
    selection: Option<Selection>,
    pending: Option<Pending>,
    /// Keyboard cursor for visual selection, in document (row, col).
    cursor: (usize, usize),
    /// Index into the rail's placed annotations.
    rail_cursor: usize,
    mode: Mode,
    /// `last`: the agent's recent messages, newest first, and the picker's cursor.
    candidates: Vec<plannotator_tui_hosts::Message>,
    pick_cursor: usize,
    /// Minutes east of UTC used to draw message times. Pinned in tests so the picker
    /// renders the same on any machine.
    clock_offset: i32,
    /// The candidate currently on screen, and the one Esc goes back to.
    pick_open: usize,
    pick_return: usize,
    /// Documents already built for candidates. Previewing swaps `open`, and a reply
    /// review's annotations live only in memory, so the one being left is kept here
    /// rather than dropped.
    pick_cache: HashMap<usize, Open>,
    message_host: String,
    /// The transcript path, for the archive's `transcript`; never the session id.
    message_transcript: String,
    /// The host-assigned session id, for the archive's `session`; never a path.
    message_session: Option<String>,
    compose: Compose,
    /// Whether the terminal reports Shift+Enter distinctly (kitty keyboard protocol).
    pub(super) shift_enter: bool,
    /// The last primary-button press, for double-click detection.
    last_click: Option<(std::time::Instant, u16, u16)>,
    geometry: Geometry,
    status: Option<String>,
    frame_ms: f64,
    frame_max_ms: f64,
    /// Copy selections to the terminal clipboard (off for headless runs).
    pub(crate) clipboard: bool,
    pub(crate) quit: bool,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("source", &self.open.source.name)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl App {
    pub(crate) fn open(source: DocumentSource, width: usize, delivery: Box<dyn Delivery>) -> Result<Self> {
        let data_dir = workspace_paths::data_dir();
        let folder = match &source.provenance {
            Provenance::File { path } => path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };
        let project = workspace_paths::project_name(&folder);
        let open = Open::new(source, width, &data_dir, &project)?;
        let send_state = if open.store.all_delivered() { SendState::Sent } else { SendState::Ready };
        Ok(Self {
            open,
            data_dir,
            project,
            tree: None,
            tree_cursor: 0,
            tree_scroll: 0,
            tree_visible: None,
            delivery,
            send_state,
            folder_counts: HashMap::new(),
            unreadable_files: Vec::new(),
            undo_archive: Vec::new(),
            archive_items: Vec::new(),
            archive_cursor: 0,
            menu_cursor: 0,
            focus: Focus::Document,
            scroll: 0,
            selected: 0,
            selection: None,
            pending: None,
            cursor: (0, 0),
            rail_cursor: 0,
            mode: Mode::Browse,
            candidates: Vec::new(),
            pick_cursor: 0,
            clock_offset: pick::local_offset_minutes(),
            pick_open: 0,
            pick_return: 0,
            pick_cache: HashMap::new(),
            changes_open: None,
            changes_counts: None,
            file_selected: 0,
            file_scroll: 0,
            message_host: String::new(),
            message_transcript: String::new(),
            message_session: None,
            compose: Compose::default(),
            shift_enter: false,
            last_click: None,
            geometry: Geometry::default(),
            status: None,
            frame_ms: 0.0,
            frame_max_ms: 0.0,
            clipboard: false,
            quit: false,
        })
    }

    /// Folder mode: a lazy tree on the left, the shallowest Markdown file open. A folder
    /// with none near the top opens on a placeholder so the tree is still browsable.
    pub(crate) fn open_folder(root: &Path, width: usize, delivery: Box<dyn Delivery>) -> Result<Self> {
        let mut tree = Tree::scan(root)?;
        let first = crate::tree::first_file_shallow(root, 2_000);
        let source = match &first {
            Some(path) => read_file(path)?,
            None => DocumentSource::new(
                format!(
                    "# {}\n\nNo Markdown file found near the top of this folder.\n\nPick one from the tree on the left: `Tab` focuses it, `Enter` opens a file or expands a folder.\n",
                    root.display()
                ),
                root.display().to_string(),
                true,
                Provenance::Stdin,
            ),
        };
        let mut app = Self::open(source, width, delivery)?;
        // The project is the folder's, not the first file's parent's.
        app.project = workspace_paths::project_name(root);
        if let Some(path) = &first {
            let open = Open::new(read_file(path)?, width, &app.data_dir, &app.project)?;
            app.set_open(open);
        }
        app.refresh_counts(&mut tree);
        app.tree_cursor = first.as_deref().and_then(|p| tree.position(p)).unwrap_or(0);
        app.tree = Some(tree);
        app.refresh_review_counts();
        app.derive_send_state();
        Ok(app)
    }

    /// Recompute the tree's annotation counts from the records on disk.
    fn refresh_counts(&self, tree: &mut Tree) {
        let (data_dir, project) = (self.data_dir.clone(), self.project.clone());
        tree.set_counts(|path| Store::count_at(&Location::for_file(&data_dir, &project, path)));
    }

    /// Keep the tree's counts current after any annotation change.
    fn sync_tree_counts(&mut self) {
        if let Some(mut tree) = self.tree.take() {
            self.refresh_counts(&mut tree);
            self.tree = Some(tree);
        }
    }

    /// Whether the tree is drawn at `width` columns: explicit toggle wins, else by width.
    pub(super) fn tree_shown(&self, width: u16) -> bool {
        self.tree.is_some() && self.tree_visible.unwrap_or(width >= draw::TREE_MIN_TOTAL_WIDTH)
    }

    fn toggle_tree(&mut self, width: u16) {
        let shown = self.tree_shown(width);
        self.tree_visible = Some(!shown);
        if shown && self.focus == Focus::Tree {
            self.focus = Focus::Document;
        }
    }

    /// Open the file under the tree cursor, or expand/collapse a directory.
    fn open_tree_selection(&mut self) -> Result<()> {
        let Some(row) = self.tree.as_ref().and_then(|t| t.rows.get(self.tree_cursor)) else { return Ok(()) };
        if row.is_dir {
            if let Some(mut tree) = self.tree.take() {
                let result = tree.toggle(self.tree_cursor);
                self.refresh_counts(&mut tree);
                self.tree = Some(tree);
                result?;
                self.refresh_review_counts();
                self.derive_send_state();
            }
            return Ok(());
        }
        let path = row.path.clone();
        if matches!(&self.open.source.provenance, Provenance::File { path: p } if *p == path) {
            self.focus = Focus::Document;
            return Ok(());
        }
        let width = self.open.layout.width;
        let open = Open::new(read_file(&path)?, width, &self.data_dir, &self.project)?;
        self.set_open(open);
        self.update_open_review_counts();
        self.derive_send_state();
        self.scroll = 0;
        self.selected = 0;
        self.cursor = (0, 0);
        self.rail_cursor = 0;
        self.clear_selection();
        self.focus = Focus::Document;
        Ok(())
    }

    pub(crate) fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    pub(crate) fn record_frame(&mut self, ms: f64) {
        self.frame_ms = if self.frame_ms == 0.0 { ms } else { self.frame_ms * 0.9 + ms * 0.1 };
        self.frame_max_ms = self.frame_max_ms.max(ms);
    }

    /// Annotate a source range: the rendered text is derived from the layout so the
    /// Workspaces web client can find it. Saved immediately.
    fn annotate(&mut self, range: Range<usize>, kind: Kind, body: String) -> Result<()> {
        let rendered = self.open.layout.rendered_in_range(&self.open.doc.source, &range);
        self.open.store.add(&self.open.doc, range, rendered, kind, body)?;
        self.capture_baseline();
        self.mark_unsent();
        self.sync_tree_counts();
        Ok(())
    }

    /// Persist the content under review so a later open can show what the agent changed
    /// since this round. Best-effort: a failed baseline write degrades to "no diff
    /// overlay" and must never fail the annotation itself. Skipped while a diff is
    /// active — comments on the changes must not clobber the version they diff against.
    fn capture_baseline(&mut self) {
        if self.changes_open.is_some()
            || self.open.overlay.is_some()
            || self.open.source.transient
            || !matches!(self.open.source.provenance, Provenance::File { .. })
        {
            return;
        }
        if let Some(record) = self.open.store.location_record() {
            let _ = overlay::write(record, &self.open.doc.source);
            // A fresh round makes the current content the reviewed version too, so a
            // region accept later can be un-accepted back to it.
            let _ = overlay::write_reviewed(record, &self.open.doc.source);
        }
    }

    /// The only sanctioned way to replace the open document: drops the changes-view
    /// stash, which is valid for exactly the Open it was made from. A tree switch,
    /// folder open, or reload that bypasses this would leave `i` restoring a foreign
    /// document.
    fn set_open(&mut self, open: Open) {
        self.open = open;
        self.changes_open = None;
        self.changes_counts = None;
    }

    /// Whether the synthesized whole-file diff is on screen instead of the file review.
    fn in_changes_view(&self) -> bool {
        self.changes_open.is_some()
    }

    /// `i`: swap the file review for the whole-file diff, or back. The stash keeps the
    /// review intact — annotations on it live on disk, the swap is purely visual.
    fn toggle_changes_view(&mut self) -> Result<()> {
        if let Some(md) = self.changes_open.take() {
            self.changes_counts = None;
            self.open = md;
            self.swap_open_reset();
            // Return to the block and offset the review was left at, so a region verb
            // pressed after inspecting the diff acts on the block the user picked.
            self.selected = self.file_selected.min(self.open.doc.blocks.len().saturating_sub(1));
            self.scroll = self.file_scroll.min(self.open.layout.total_rows.saturating_sub(1));
            self.ensure_selected_visible();
            self.status = Some("file review".into());
            return Ok(());
        }
        let Some(overlay) = &self.open.overlay else {
            self.status = Some("no changes yet: the file matches your last review".into());
            return Ok(());
        };
        let name = self.open.source.name.clone();
        let diff_text = overlay.full_diff.clone();
        let counts = (overlay.added_lines, overlay.removed_lines);
        let source = DocumentSource::new(
            diff_text,
            format!("{name} · changes"),
            true,
            Provenance::File { path: self.open_source_path().unwrap_or_default() },
        );
        let source = Self::as_diff_format(source);
        let width = self.open.layout.width;
        let changes = Open::new(source, width, &self.data_dir, &self.project)?;
        let md = std::mem::replace(&mut self.open, changes);
        // A region verb from the diff acts on the file's block: keep the block the user
        // picked, or default to the first changed block when the cursor has none.
        let picked = self.selected;
        let target = md.overlay.as_ref().and_then(|ov| {
            if md.doc.blocks.get(picked).is_some_and(|b| ov.touches(&b.range)) {
                Some(picked)
            } else {
                md.doc.blocks.iter().position(|b| ov.touches(&b.range))
            }
        });
        self.changes_open = Some(md);
        self.file_selected = target.unwrap_or(picked);
        self.file_scroll = self.scroll;
        self.swap_open_reset();
        self.changes_counts = Some(counts);
        self.status = Some(format!("whole-file diff · +{} −{} · i back to file", counts.0, counts.1));
        Ok(())
    }

    fn open_source_path(&self) -> Option<PathBuf> {
        match &self.open.source.provenance {
            Provenance::File { path } => Some(path.clone()),
            _ => None,
        }
    }

    /// `DocumentSource::new` defaults to Markdown; the changes view parses as a diff.
    fn as_diff_format(mut source: DocumentSource) -> DocumentSource {
        source.format = SourceFormat::Diff;
        source
    }

    /// Reset per-document cursor state after the caller replaced `self.open`
    /// (same reset list as the picker's `show_candidate`). Tree state is preserved —
    /// the folder the user is in did not change.
    fn swap_open_reset(&mut self) {
        self.scroll = 0;
        self.selected = 0;
        self.cursor = (0, 0);
        self.rail_cursor = 0;
        self.clear_selection();
        self.focus = Focus::Document;
        self.derive_send_state();
    }

    /// `a`: fold the hovered block's changed lines into the baseline. Other changed
    /// blocks keep their marks. From the changes view, return to the file first — the
    /// verbs operate on the review, never on the transient diff document.
    fn accept_region_at_cursor(&mut self) -> Result<()> {
        if self.in_changes_view() {
            self.toggle_changes_view()?;
        }
        let Some(block) = self.hovered_block() else {
            self.status = Some("no block under the cursor".into());
            return Ok(());
        };
        let Some(overlay) = self.open.overlay.as_ref() else {
            self.status = Some("nothing to accept: the file matches your last review".into());
            return Ok(());
        };
        let updated = overlay::accept_region(&overlay.baseline, &self.open.doc.source, &block);
        if updated == overlay.baseline {
            self.status = Some("no changes in this block to accept".into());
            return Ok(());
        }
        let Some(record) = self.open.store.location_record().map(Path::to_path_buf) else {
            self.status = Some("nothing to accept: annotations are not persisted".into());
            return Ok(());
        };
        overlay::write(&record, &updated)?;
        self.reload()?;
        self.status = Some("accepted this block · other changes still marked".into());
        Ok(())
    }

    /// `A`: the whole file is fine. Fold everything into the baseline so no marks remain.
    fn accept_all(&mut self) -> Result<()> {
        if self.in_changes_view() {
            self.toggle_changes_view()?;
        }
        if self.open.overlay.is_none() {
            self.status = Some("nothing to accept: the file matches your last review".into());
            return Ok(());
        }
        if let Some(record) = self.open.store.location_record() {
            overlay::write(record, &self.open.doc.source)?;
            // Accepting everything makes the current file the reviewed version, so
            // un-accept has nothing to go back to until the next round.
            overlay::write_reviewed(record, &self.open.doc.source)?;
        }
        self.open.overlay = None;
        self.status = Some("all changes accepted · baseline updated".into());
        Ok(())
    }

    /// `D`: put the hovered block back to the reviewed version and re-read. From the
    /// changes view, return to the file first — revert is a review verb.
    fn revert_region_at_cursor(&mut self) -> Result<()> {
        if self.in_changes_view() {
            self.toggle_changes_view()?;
        }
        let Some(block) = self.hovered_block() else {
            self.status = Some("no block under the cursor".into());
            return Ok(());
        };
        let Some(overlay) = self.open.overlay.as_ref() else {
            self.status = Some("nothing to revert: the file matches your last review".into());
            return Ok(());
        };
        let Provenance::File { path } = &self.open.source.provenance else {
            return Ok(());
        };
        let path = path.clone();
        let reverted = overlay::revert_region(&overlay.baseline, &self.open.doc.source, &block);
        if reverted == self.open.doc.source {
            self.status = Some("no changes in this block to revert".into());
            return Ok(());
        }
        std::fs::write(&path, reverted).with_context(|| format!("reverting {}", path.display()))?;
        self.reload()?;
        self.status = Some("reverted this block to the version you reviewed".into());
        Ok(())
    }

    /// `X`: put the whole file back to the reviewed version and re-read it. Annotations
    /// were made against exactly that content, so they re-anchor cleanly.
    fn revert_all(&mut self) -> Result<()> {
        if self.in_changes_view() {
            self.toggle_changes_view()?;
        }
        let Some(overlay) = &self.open.overlay else {
            self.status = Some("nothing to revert: the file matches your last review".into());
            return Ok(());
        };
        let Provenance::File { path } = &self.open.source.provenance else {
            return Ok(());
        };
        let path = path.clone();
        let baseline = overlay.baseline.clone();
        std::fs::write(&path, baseline).with_context(|| format!("reverting {}", path.display()))?;
        self.reload()?;
        self.status = Some("reverted to the version you reviewed".into());
        Ok(())
    }

    /// `U`: put the reviewed text back into the baseline for the hovered block, so a
    /// block that was accepted shows as changed again. The round's original content
    /// lives in `reviewed.md`, untouched by region accepts.
    fn unaccept_region_at_cursor(&mut self) -> Result<()> {
        if self.in_changes_view() {
            self.toggle_changes_view()?;
        }
        let Some(block) = self.hovered_block() else {
            self.status = Some("no block under the cursor".into());
            return Ok(());
        };
        let Some(overlay) = self.open.overlay.as_ref() else {
            self.status = Some("nothing to un-accept: the file matches your last review".into());
            return Ok(());
        };
        let Some(record) = self.open.store.location_record().map(Path::to_path_buf) else {
            return Ok(());
        };
        let Some(reviewed) = overlay::read_reviewed(&record) else {
            self.status = Some("nothing to un-accept: no reviewed version recorded".into());
            return Ok(());
        };
        let restored = overlay::restore_region(&reviewed, &overlay.baseline, &self.open.doc.source, &block);
        if restored == overlay.baseline {
            self.status = Some("nothing accepted in this block".into());
            return Ok(());
        }
        overlay::write(&record, &restored)?;
        self.reload()?;
        self.status = Some("un-accepted this block · it shows as changed again".into());
        Ok(())
    }

    /// The byte range of the block under the cursor, if any.
    fn hovered_block(&self) -> Option<Range<usize>> {
        self.open.doc.blocks.get(self.selected).map(|b| b.range.clone())
    }

    /// Whether the diff review verbs are live: an overlay on the file, or its changes
    /// view on screen (the overlay is stashed while that view is up).
    fn overlay_active(&self) -> bool {
        self.open.overlay.is_some() || self.in_changes_view()
    }

    /// Apply a toolbar action to the pending selection.
    fn act(&mut self, kind: Kind) -> Result<()> {
        let Some(pending) = self.pending.clone() else { return Ok(()) };
        match kind {
            Kind::Comment => {
                self.mode = Mode::Compose;
                self.compose = Compose::default();
            }
            Kind::LooksGood | Kind::Delete => {
                self.annotate(pending.range, kind, String::new())?;
                self.clear_selection();
                self.status = Some(format!("{} saved", label(kind)));
            }
        }
        Ok(())
    }

    /// Begin editing the body of the annotation under the rail cursor.
    fn edit_selected_annotation(&mut self) {
        let placed = self.open.store.placed();
        let Some(target) = placed.get(self.rail_cursor) else { return };
        self.compose = Compose::with_text(&target.annotation.body);
        self.mode = Mode::Edit(target.annotation.id.clone());
    }

    fn remove_selected_annotation(&mut self) -> Result<()> {
        let id = self.open.store.placed().get(self.rail_cursor).map(|p| p.annotation.id.clone());
        let Some(id) = id else { return Ok(()) };
        if self.open.store.remove(&id)? {
            self.mark_unsent();
            self.status = Some("annotation removed".into());
            self.rail_cursor = self.rail_cursor.min(self.open.store.placed().len().saturating_sub(1));
            self.sync_tree_counts();
        }
        Ok(())
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.pending = None;
    }

    fn select_block(&mut self, block: usize) {
        if self.open.doc.blocks.is_empty() {
            return;
        }
        self.clear_selection();
        self.selected = block.min(self.open.doc.blocks.len() - 1);
        if let Some(rendered) = self.open.layout.blocks.get(self.selected) {
            self.cursor = (rendered.first_row, 0);
        }
        self.ensure_selected_visible();
    }

    fn ensure_selected_visible(&mut self) {
        let height = usize::from(self.geometry.doc.height.max(1));
        let Some(block) = self.open.layout.blocks.get(self.selected) else { return };
        let first = block.first_row;
        let last = first + block.rows.len().saturating_sub(1);
        if first < self.scroll {
            self.scroll = first.saturating_sub(1);
        } else if last >= self.scroll + height {
            self.scroll = (last + 2).saturating_sub(height).min(first);
        }
    }

    fn ensure_cursor_visible(&mut self) {
        let height = usize::from(self.geometry.doc.height.max(1));
        if self.cursor.0 < self.scroll {
            self.scroll = self.cursor.0;
        } else if self.cursor.0 >= self.scroll + height {
            self.scroll = self.cursor.0 + 1 - height;
        }
    }

    fn scroll_by(&mut self, delta: i64) {
        let height = usize::from(self.geometry.doc.height.max(1));
        let max = self.open.layout.total_rows.saturating_sub(height);
        self.scroll = (self.scroll as i64 + delta).clamp(0, max as i64) as usize;
    }

    fn tree_len(&self) -> usize {
        self.tree.as_ref().map_or(0, |t| t.rows.len())
    }

    /// Scroll the tree by `delta` rows without moving its cursor (mouse wheel over the tree).
    fn tree_scroll_by(&mut self, delta: i64) {
        let height = usize::from(self.geometry.tree.height.max(1));
        let max = self.tree_len().saturating_sub(height);
        self.tree_scroll = (self.tree_scroll as i64 + delta).clamp(0, max as i64) as usize;
    }

    /// Move the tree's window so `tree_cursor` is inside its `height` visible rows.
    fn keep_tree_cursor_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.tree_cursor < self.tree_scroll {
            self.tree_scroll = self.tree_cursor;
        } else if self.tree_cursor >= self.tree_scroll + height {
            self.tree_scroll = self.tree_cursor + 1 - height;
        }
        self.tree_scroll = self.tree_scroll.min(self.tree_len().saturating_sub(height));
    }

    /// Re-read the document from its provenance and re-resolve every annotation.
    fn reload(&mut self) -> Result<()> {
        if self.in_changes_view() {
            // The changes view is a snapshot of one read; the file under it is what
            // reloads. Return to it first so the re-read lands on the right document.
            self.toggle_changes_view()?;
        }
        if self.open.source.format == SourceFormat::Diff {
            // A standalone .patch file is a snapshot of one agent edit; a regenerated
            // patch reopens as a new review instead of silently re-anchoring old
            // comments. The changes view never reaches this line — it swapped out
            // above and the file review it belongs to reloads normally.
            self.status = Some("a diff is a snapshot; open the new patch to review it".into());
            return Ok(());
        }
        let Provenance::File { path } = &self.open.source.provenance else {
            self.status = Some("not a file; nothing to reload".into());
            return Ok(());
        };
        let path = path.clone();
        self.open.source = read_file(&path)?;
        self.open.doc = Document::parse(self.open.source.content.clone());
        self.open.layout = DocLayout::build(&self.open.doc, self.open.layout.width);
        self.open.store =
            Store::load(&Location::for_file(&self.data_dir, &self.project, &path), &self.open.doc)?;
        self.open.overlay = Open::diff_overlay(&self.open.source, &self.open.store);
        // The re-read file makes any stashed changes view stale by definition.
        self.changes_open = None;
        self.changes_counts = None;
        self.refresh_review_counts();
        self.derive_send_state();
        self.sync_tree_counts();
        self.clear_selection();
        self.selected = self.selected.min(self.open.doc.blocks.len().saturating_sub(1));
        let mut status = format!("reloaded · {} orphaned", self.open.store.orphans());
        if let Some(note) = self.unreadable_note() {
            status = format!("{status} · {note}");
        }
        self.status = Some(status);
        Ok(())
    }

    // ----- headless helpers (bench, snapshot, scripting) -----------------------------

    /// Annotate a whole block by index.
    pub(crate) fn add_block_annotation(&mut self, block: usize, kind: Kind, body: String) -> Result<()> {
        let range = self.open.doc.blocks.get(block).map(|b| b.range.clone());
        let range = range.ok_or_else(|| {
            anyhow::anyhow!("block {block} out of range ({} blocks)", self.open.doc.blocks.len())
        })?;
        self.annotate(range, kind, body)
    }

    /// Annotate the first occurrence of `quote` in the source.
    pub(crate) fn add_quote_annotation(&mut self, quote: &str, kind: Kind, body: String) -> Result<()> {
        let start =
            self.open.doc.source.find(quote).ok_or_else(|| anyhow::anyhow!("quote not found: {quote:?}"))?;
        self.annotate(start..start + quote.len(), kind, body)
    }

    pub(crate) fn describe_blocks(&self) -> Vec<String> {
        self.open
            .doc
            .blocks
            .iter()
            .zip(&self.open.layout.blocks)
            .enumerate()
            .map(|(i, (block, rendered))| {
                let first = self.open.doc.block_text(i).lines().next().unwrap_or("");
                let head: String = first.chars().take(60).collect();
                format!("{i:4} {:<10} row {:>5}  {head}", format!("{:?}", block.kind), rendered.first_row)
            })
            .collect()
    }

    /// Scroll and select the first visible block.
    pub(crate) fn scroll_for_snapshot(&mut self, delta: i64) {
        self.scroll_by(delta);
        if let Some(block) = self.open.layout.block_at_row(self.scroll) {
            self.selected = block;
        }
    }

    /// Simulate a finished drag over the first occurrence of `quote`.
    pub(crate) fn select_quote_for_snapshot(&mut self, quote: &str) -> Result<()> {
        let start =
            self.open.doc.source.find(quote).ok_or_else(|| anyhow::anyhow!("quote not found: {quote:?}"))?;
        let range = start..start + quote.len();
        let cells = self.open.layout.blocks.iter().flat_map(|b| {
            b.rows.iter().enumerate().flat_map(move |(ri, row)| {
                row.cells.iter().enumerate().map(move |(col, cell)| ((b.first_row + ri, col), *cell))
            })
        });
        let hits: Vec<_> =
            cells.filter(|(_, cell)| cell.is_some_and(|o| range.contains(&o))).map(|(pos, _)| pos).collect();
        let first = *hits.first().ok_or_else(|| anyhow::anyhow!("quote is not rendered"))?;
        let last = hits.last().copied().unwrap_or(first);
        self.selection = Some(Selection::finished(first, last));
        self.finish_selection();
        Ok(())
    }
}

fn read_file(path: &Path) -> Result<DocumentSource> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(DocumentSource::file(PathBuf::from(path), content))
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Comment => "comment",
        Kind::LooksGood => "looks good",
        Kind::Delete => "delete this",
    }
}

fn glyph(kind: Kind) -> &'static str {
    match kind {
        Kind::Comment => "💬",
        Kind::LooksGood => "👍",
        Kind::Delete => "✗",
    }
}
