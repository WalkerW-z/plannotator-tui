# Spec: task board

## Why

A note is already where work is written down. Today the note and the agent session are not
connected: the human re-states tasks in prose to dispatchers, and completion is recorded by
hand. This spec makes one Markdown file the queue, with the terminal board as its surface and
a runner extension as the only writer of task state.

## Shape

Two components, one product, two shared artifacts.

- **Board** — this repository after the fork. Renders the note as a task board. Never writes
  task state.
- **Runner** — a pi extension at `pi/delegate/`. Reads intent files, dispatches child agents,
  writes task state.
- **Shared artifacts** — the note file (source of truth) and intent files (board → runner).

```text
note.md ──read──> board ──intent.json──> runner ──pi-subagents──> child agents
   ^                                       │
   └───────────── task state (runner only) ┘
```

## 1. Note contract

A **task** is a top-level GFM task-list item: a depth-0 list item carrying a task marker.
Detection uses the `pulldown-cmark` event stream with `Options::ENABLE_TASKLISTS`; no
hand-rolled Markdown interpretation. Extraction lives in a new module, not in `doc.rs` whose
contract is "nothing here knows what a heading or a list *is*".

**File states — exactly two.**

| Marker | Meaning | Written by |
| --- | --- | --- |
| `- [ ]` | open | whoever wrote the note |
| `- [x]` | done | the runner |

In-flight, blocked and failed are **not** represented in the file. They live in the sidecar and
are rendered by the board. A note opened in any other editor therefore shows only committed
truth.

**Identity.** The note stays id-free. A task id is 8 hex characters of SHA-256 over
`heading_path + "\u{1f}" + normalized_text`, where `normalized_text` is the task text with
whitespace runs collapsed to single spaces and a trailing evidence suffix removed
(`\s+[—-]\s+[0-9a-f]{7,40}$`), and `heading_path` is the `/`-joined chain of enclosing heading
texts (empty when none). Removing the evidence suffix is required: the runner appends evidence
to the same line, and an unstable hash would re-dispatch completed work.

**Location.** Ids are re-derived from the file on every read. Nothing is cached across runs.

**Orphans.** An in-flight sidecar entry whose id matches no current task (text edited, line
removed) is rendered in a separate "in flight, not found" group and is never re-attached by
fuzzy matching. The runner reports it; a human decides.

**Precedence.** File `[x]` wins over a sidecar in-flight entry. The runner reconciles
idempotently: a task a human ticked is recorded done, and a late completion writes nothing
twice.

**Evidence.** On completion the runner appends ` — <short-sha>` when the work produced a
commit, else ` — run <run-id>`. Appending is idempotent: a line already carrying that evidence
is left alone.

## 2. Sidecar

Location: beside the annotation record, using the existing data dir and project keys —
`annotations_dir(data_dir, project, resolved_path)` then a sibling `tasks.json`.

```json
{
  "v": 1,
  "note": "/abs/path/to/note.md",
  "updated_at": "2026-09-21T09:00:00Z",
  "items": [
    {
      "id": "a1b2c3d4",
      "heading_path": "Edit Module",
      "text_hash": "<sha256 hex of normalized_text>",
      "text": "task text at dispatch",
      "state": "in_flight | blocked | failed | done",
      "workflow": "investigate | build | review",
      "intent": ".pi/delegate/intents/2026-09-21T09-00-00Z-a1b2c3d4.json",
      "run_id": "…",
      "mission_id": "…",
      "branch": "pi-agent-a1b2c3d4",
      "evidence": "a1b2c3d",
      "updated_at": "2026-09-21T09:00:00Z"
    }
  ]
}
```

`branch` is present for `build`. `reason` is present when `state` is `blocked` or `failed`.

Written only by the runner. Read-only for the board.

**Failure policy differs by direction.** For *display*, a missing or corrupt sidecar means "no
task is in flight" — the human's view must not be gated by an unreadable machine file. For
*dispatch*, a missing or corrupt sidecar means refuse: dispatching without the in-flight record
risks doing the same work twice.

## 3. Board surface

**Modes.** Two added, both mirroring shapes that already exist: `Mode::TaskMenu` (verbs for the
cursor's task, drawn like the message picker) and `Mode::TaskPrompt` (follow-up compose,
reusing the existing text path).

**Keys in Browse, cursor on a task.**

| Key | Action |
| --- | --- |
| `i` | investigate — write intent, deliver |
| `b` | build — write intent, deliver |
| `r` | review — write intent, deliver |
| `enter` | follow-up prompt (annotation anchored to the task) |
| `e` | edit the note in the configured editor |
| `m` | open `TaskMenu` |

**Locked tasks.** When the sidecar says in flight: the row is dimmed and glyph-marked; `i`, `b`
and `r` are inert and the footer names the reason; `enter` and `e` remain available — a human
may always comment on running work and edit the note.

**Follow-ups are annotations.** A follow-up is an annotation anchored to the task's byte range,
so persistence, edit-survival, and numbered-Markdown export are the existing mechanisms. No new
storage.

**Watch.** Poll the note's mtime and length on the event tick (500 ms). On change, reload and
re-anchor the cursor by task id, falling back to the nearest index when the id is gone.

**State added to `App`.** `tasks: Vec<Task>`, `task_cursor: usize`, `sidecar: Option<Sidecar>`,
`note: PathBuf`, `last_mtime: Option<SystemTime>`.

**Config.** New sections, both strict (`deny_unknown_fields` already makes typos loud):

```toml
[editor]
command = "nvim"        # else $VISUAL, $EDITOR, then "vi"

[keys]
investigate = "i"
build = "b"
review = "r"
```

**Editor round-trip.** Leave the alternate screen and raw mode, spawn the editor on the note,
wait, re-enter, reload. Errors return `Result`; the terminal is restored on the error path.

**CLI.** `plannotator-tui tasks <note.md>` opens the board. The Herdr manifest gains one action
("Task board: open here") beside the annotator's.

## 4. Intent bridge

Directory: `<workspace root>/.pi/delegate/intents/`, where the workspace root is the git
toplevel (existing `workspace_paths.rs`) or the note's directory outside a repository.
Filename: `<utc compact>-<task id>.json`.

```json
{
  "v": 1,
  "note": "/abs/path/note.md",
  "task_id": "a1b2c3d4",
  "heading_path": "Edit Module",
  "text": "task text as dispatched",
  "workflow": "build",
  "follow_up": "",
  "base_ref": "HEAD",
  "created_at": "2026-09-21T09:00:00Z"
}
```

Delivery reuses the existing `Delivery` seam: a Herdr agent-pane prompt carrying a pointer —
`/delegate-intent .pi/delegate/intents/<name>.json` — with the clipboard fallback, and `Discard`
when headless. **The prompt carries no contract; the file does.** The file is re-readable after
a restart, versionable, and testable.

`v` is normative. The schema lives in `docs/intent-schema.md`; a checked-in fixture
`docs/fixtures/intent-v1.json` is written by a Rust test and validated by the runner's test, so
the two languages cannot drift silently.

## 5. Runner

`/delegate-intent <path>` — read, require `v == 1`, then:

1. Re-derive task ids from the note; locate the id.
2. Refuse when the sidecar already has the id in flight (idempotent no-op).
3. Record `in_flight`; dispatch.
4. On completion: flip the marker to `- [x]`, append evidence once, record `done`.
5. On failure: **leave the file as `- [ ]`**, record `failed` with a reason in the sidecar, and
   report. Failure is never written into the note.

**Workflows** map onto the already-installed `pi-subagents` engine:

| Verb | Dispatch |
| --- | --- |
| `investigate` | read-only recon child, no worktree; result summarized into the sidecar |
| `build` | worker child with `isolation: "worktree"`, then a reviewer child on its branch |
| `review` | reviewer child over the current diff or named branch |

**Serialization.** `build` intents serialize per repository, matching the one-writer rule. A
`build` intent that arrives while another is running is **refused, not queued**: the runner
records `blocked` with a reason and reports it, and the human re-issues once the previous build
lands. `investigate` and `review` may run concurrently.

**Not found is not a guess.** If the task cannot be located in the note, the runner stops and
reports; it never dispatches against a fuzzy match.

**Embedded tooling.** Children that need extension-provided tools (browser, context-mode, lens)
require those providers registered in `subagentOnlyExtensions`/`extensions` for the child
runtime; without it such children fail at launch. This is configuration, and it is verified in
the runner's setup check rather than discovered at dispatch time.

## 6. Scope

**Kept:** the schema crate (Workspaces wire shape), the hosts crate including `pi.rs`, the
delivery seam (Herdr pane prompt, clipboard), annotations with the rail and compose/edit,
the send path, `last` mode, config, `--bench`, and the workspace lints.

**Removed:** folder mode (`tree.rs`, `App::open_folder`, folder counts, the folder send scope),
the archive browser (`app/archive_view.rs` and its restore path), the diff-document mode
(`is_patch`, `BlockKind::DiffFileHeader`, the round-2 diff view), and the baseline overlay
(`overlay.rs`, baseline capture, accept/revert/inspect) with its tests.

**Split, not deleted:** `store/review.rs`. Its delivery-coverage half (`Delivered`) is used by
the send path and stays; the finished-review restore half goes with the archive.

**Assumption to confirm:** `archive.rs` writes the shared feedback index
(`{data_dir}/feedback/{project}/index.jsonl`, schema v1 frozen at `443e1fcf` in Plannotator) as
part of sending. Removing the archive *browser* does not require removing that *write*, and this
spec keeps the write so externally archived feedback stays readable. Say so if the write should
go too.

Estimated removal: on the order of 1,400–1,600 of the app crate's 9,112 lines, plus the `App`
fields and modes that exist only to serve them. Final number comes from the implementation, not
from this estimate.

## 7. Phasing

Each phase is independently testable.

1. **P1 board only.** Parse, render, lock from a fixture sidecar, `$EDITOR`, watch. Acceptance:
   fixtures covering nesting, multiple headings and malformed items parse correctly; a locked
   row refuses the three verbs and allows follow-up; an edit round-trip restores the TUI with
   the cursor on the same task; **the board writes nothing to the note** (assert the file's bytes
   are unchanged across a session, excluding a human's own edit through `e`).
2. **P2 bridge.** Intents, delivery, `in_flight`. Acceptance: one keypress writes exactly one
   intent file and delivers one pointer prompt; the note is still untouched by the board; the
   board shows the lock within one poll.
3. **P3 dispatch and writeback.** Acceptance: `[x]` plus evidence appears exactly once and
   re-running is a no-op; failure leaves `[ ]` with a sidecar reason; a second intent for the
   same id is refused; an orphaned in-flight entry is surfaced, not re-attached.
4. **P4 follow-up.** Acceptance: a follow-up on a locked task reaches the running child as
   steering and starts no new child.
5. **P5 landing.** Merge sequence for accumulated branches. Deliberately unspecified here.

The first implementation plan covers P1–P3. P4 and P5 get their own plans once P3 has run on a
real note.

## 8. Not built

Task dependencies (`Blocked by`), scheduling, boards spanning many notes, cross-repository
fleets, merge automation before P5, upstream tracking of `plannotator/plannotator-tui`, task ids
written into the note, and human diff review inside the board (diffs are reviewed by the `review`
verb's child; human review happens outside the board or through the retained `last` flow).

## 9. Open items for the owner

1. **Product name.** A hard fork needs its own identity. Renaming touches: the binary name, the
   three crate names, `~/.config/plannotator-tui/config.toml`, `$PLANNOTATOR_TUI_CONFIG`,
   `PLANNOTATOR_TUI_PLACEMENT`, the Herdr plugin id and manifest, and the `client` field written
   into the shared feedback index. Choose the name before the rename lands; the spec does not
   depend on it.
2. **The `archive.rs` write** — see the assumption in Scope.
