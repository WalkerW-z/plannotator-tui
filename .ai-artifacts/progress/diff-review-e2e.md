# diff-review-e2e

## Status
active

## Objective
Test the post-0.8.0 review cluster end to end, give the human a hands-on way to drive it, and fix what the tests surface.

## State
- Landing: a changed file opens on the file review (marks + chip); `i` opens the whole-file diff on demand. `enter_changes_view_if_any` removed; its 4 test callers use `toggle_changes_view`.
- Harness/scripts: `--snapshot` takes `keys=<spec>`; `scripts/e2e-review.sh` = 36 checks pass (incl. a tmux landing check); `scripts/try-review.sh` = hands-on launcher.
- Docs: three `diff-review-*` files; `docs/decisions.md` 16.
- Clean: `cargo fmt`, `clippy --workspace --all-targets`, `test --workspace`.

## Decisions
- A changed file opens on the file review; the whole-file diff is opt-in (`i`). The diff is one synthesized `DiffHunk` with no Markdown block structure, so a verb there targets a program-chosen block. `docs/decisions.md` 16.
- Interactive paths are driven through `--snapshot keys=<spec>`, which feeds real key events through `App::handle_event`; no separate `--review-verb`.
- Tests assert on-disk state (`baseline.md`, `reviewed.md`, the file) and frame text. The `--snapshot` mark map tracks annotation kinds, not the diff overlay, so it cannot assert overlay marks.
- Hands-on state persists under `$PLANNOTATOR_TRY_DIR` with its own data dir.

## Constraints
- The crate has no lib target: integration tests drive the binary, so interactive-only paths need `keys=` or a real terminal (tmux).
- Headless commands (`--export`, `--annotate`, `--snapshot`) must keep seeing the real document; the diff view is interactive-only.
- Never write `~/.plannotator`: tests and launchers set `PLANNOTATOR_DATA_DIR`.

## Assumptions
- tmux is available for the landing check; the script skips it otherwise.
- The region-verb work (`dec15`) ships in the same commit as the landing fix.

## Open Questions
- Residual D-1: inside the opt-in diff view (`i`), `j`/`k` no-op, `c` needs a manual selection, and `a`/`D` leave and target the first changed block. Give the diff block structure, or accept?
- With no overlay, `a`/`D` are not dispatched, so the "nothing to accept/revert" statuses are unreachable. Dead branches, or make them reachable?
- Commit the region verbs + landing fix + harness + scripts + docs as one change, or split?

## Next
- Decide residual D-1, then commit.
- Confirm on the human's terminal: `scripts/e2e-review.sh`, `scripts/try-review.sh round2`.

## Context
- Scope: `dc7c92f` (diff review), `bcef757` (overlay), `4ea6c98` (whole-file diff view), uncommitted region verbs (`dec15`), `60aa488` (`last --newest`).
- Reproduce residual D-1: `bash scripts/try-review.sh --setup-only round2`, then `--snapshot <plan.md> 100 30 0 "keys=ia"`; `bash scripts/try-review.sh peek` shows `baseline.md` changed.
- Fixture `plan.md` blocks: 0 `# Plan`, 1 `Alpha line.`, 2 `Beta line.`. `try-review.sh` cases: `fresh`, `round2`, `patch`, `folder`, `peek`, `reset`.
