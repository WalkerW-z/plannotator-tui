#!/usr/bin/env bash
# End-to-end test of the round-2 review features against the real binary:
# unified diff review, the changes-since-reviewed overlay, the whole-file diff view,
# the region review verbs, and `last --newest`.
#
#   bash scripts/e2e-review.sh              build debug, run every check
#   bash scripts/e2e-review.sh --no-build   use the existing target/debug binary
#   PLANNOTATOR_TUI_BIN=/path/to/bin bash scripts/e2e-review.sh --no-build
#
# State lives in a private data dir under the temp workspace, so nothing touches
# ~/.plannotator. Exit status is the number of failed checks.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${PLANNOTATOR_TUI_BIN:-$root/target/debug/plannotator-tui}"
if [ "${1:-}" != "--no-build" ]; then
  cargo build --quiet --manifest-path "$root/Cargo.toml"
fi
[ -x "$bin" ] || { echo "no binary at $bin (run without --no-build)" >&2; exit 2; }

work="$(mktemp -d "${TMPDIR:-/tmp}/plannotator-e2e.XXXXXX")"
trap 'rm -rf "$work"' EXIT
export PLANNOTATOR_DATA_DIR="$work/data"
mkdir -p "$PLANNOTATOR_DATA_DIR"

fail=0; total=0
ok()   { total=$((total+1)); echo "  ok   $1"; }
bad()  { total=$((total+1)); fail=$((fail+1)); echo "  FAIL $1" >&2; }
contains() { # contains <label> <haystack> <needle>
  if printf '%s' "$2" | grep -qF -- "$3"; then ok "$1"; else bad "$1 (expected to contain: $3)"; fi
}
excludes() { # excludes <label> <haystack> <needle>
  if printf '%s' "$2" | grep -qF -- "$3"; then bad "$1 (unexpected: $3)"; else ok "$1"; fi
}
equals() { # equals <label> <actual> <expected>
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected [$3], got [$2])"; fi
}
run() { "$@" 2>&1 || true; }

data="$PLANNOTATOR_DATA_DIR"
record=""

# Rebuild the round-2 setup: annotate a file, then let "the agent" edit two blocks.
setup_review() {
  rm -rf "$data"; mkdir -p "$data"
  printf '# Plan\n\nAlpha line.\n\nBeta line.\n' > "$work/plan.md"
  run "$bin" --annotate "$work/plan.md" "Alpha line." note "first" >/dev/null
  printf '# Plan\n\nAlpha line edited.\n\nBeta line edited.\n' > "$work/plan.md"
  record="$(dirname "$(find "$data" -name annotations.json -print -quit)")"
}

echo "== A. unified diff review"
cat > "$work/fix.patch" <<'PATCH'
diff --git a/a.md b/a.md
--- a/a.md
+++ b/a.md
@@ -1,3 +1,3 @@
 # A
 
-old line
+new line
PATCH
out="$(run "$bin" --blocks "$work/fix.patch")"
contains "patch: file header is a block" "$out" "DiffFileHeader"
contains "patch: hunk is a block" "$out" "DiffHunk"
out="$(run "$bin" --snapshot "$work/fix.patch" 80 12)"
contains "patch: hunk header renders" "$out" "@@ -1,3 +1,3 @@"
contains "patch: removed line renders" "$out" "-old line"
contains "patch: added line renders" "$out" "+new line"
before="$(find "$data" -name annotations.json | wc -l | tr -d ' ')"
run "$bin" --annotate "$work/fix.patch" "new line" note "patch comment" >/dev/null
after="$(find "$data" -name annotations.json | wc -l | tr -d ' ')"
equals "patch: review is transient (nothing persisted)" "$after" "$before"

echo "== B. changes since the reviewed version"
setup_review
if [ -f "$record/baseline.md" ]; then ok "overlay: baseline sidecar written"; else bad "overlay: baseline sidecar written"; fi
if [ -f "$record/reviewed.md" ]; then ok "overlay: reviewed sidecar written"; else bad "overlay: reviewed sidecar written"; fi
out="$(run "$bin" --snapshot "$work/plan.md" 100 30 0)"
contains "overlay: footer chip counts the change" "$out" "+2 −2 changed since your rev"
excludes "overlay: headless shows the file, not the diff" "$out" "@@"

echo "== C. whole-file diff view"
out="$(run "$bin" --snapshot "$work/plan.md" 100 30 0 "keys=i")"
contains "changes: hunk header" "$out" "@@ -1,5 +1,5 @@"
contains "changes: removed line" "$out" "-Alpha line."
contains "changes: added line" "$out" "+Alpha line edited."
contains "changes: footer" "$out" "whole-file diff · +2 −2"
out="$(run "$bin" --snapshot "$work/plan.md" 100 30 0 "keys=i<esc>")"
excludes "changes: esc leaves the diff" "$out" "@@"
contains "changes: file review returns" "$out" "Alpha line edited."

echo "== D. region review verbs"
setup_review
run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=ja >/dev/null
baseline="$(cat "$record/baseline.md")"
contains "verbs: a folds the hovered block" "$baseline" "Alpha line edited."
contains "verbs: a keeps the other block" "$baseline" "Beta line."
excludes "verbs: a did not fold block 2" "$baseline" "Beta line edited."

run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=jU >/dev/null
baseline="$(cat "$record/baseline.md")"
contains "verbs: U restores the reviewed text" "$baseline" "Alpha line."
excludes "verbs: U drops the accepted text" "$baseline" "Alpha line edited."

setup_review
run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=jjD >/dev/null
file="$(cat "$work/plan.md")"
contains "verbs: D reverts the hovered block" "$file" "Beta line."
excludes "verbs: D left the other block changed" "$file" "Beta line edited."
contains "verbs: D left block 1 accepted-side" "$file" "Alpha line edited."

setup_review
run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=A >/dev/null
equals "verbs: A folds the whole file" "$(cat "$record/baseline.md")" "$(cat "$work/plan.md")"
equals "verbs: A moves the reviewed version too" "$(cat "$record/reviewed.md")" "$(cat "$work/plan.md")"
out="$(run "$bin" --snapshot "$work/plan.md" 100 30 0)"
excludes "verbs: A clears the marks" "$out" "changed since your rev"

setup_review
run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=X >/dev/null
equals "verbs: X restores the whole file" "$(cat "$work/plan.md")" "$(cat "$record/baseline.md")"

setup_review
out="$(run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=a)"
contains "verbs: unchanged block reports nothing to accept" "$out" "no changes in this block to accept"

run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=A >/dev/null
out="$(run "$bin" --snapshot "$work/plan.md" 100 30 0 keys=U)"
contains "verbs: U after A is the review undo, not un-accept" "$out" "nothing to undo"

printf '# Fresh\n\nBody.\n' > "$work/fresh.md"
out="$(run "$bin" --snapshot "$work/fresh.md" 100 20 0 keys=a)"
excludes "verbs: no overlay means no accept" "$out" "accepted this block"

echo "== E. last --newest"
out="$(run "$bin" herdr open --newest)"
contains "newest: herdr open rejects it by name" "$out" "only for \`herdr last\`"
out="$(run "$bin" herdr open --bogus)"
contains "newest: unknown flags still error" "$out" "unknown flag --bogus"
out="$(printf 'hello newest' | run "$bin" last --stdin --print)"
contains "newest: stdin --print passes text through" "$out" "hello newest"

echo "== F. interactive landing"
if command -v tmux >/dev/null 2>&1; then
  setup_review
  session="plannotator-e2e-$$"
  tmux kill-session -t "$session" 2>/dev/null || true
  tmux new-session -d -s "$session" -x 120 -y 28 "PLANNOTATOR_DATA_DIR=$data $bin $work/plan.md"
  sleep 1.5
  landing="$(tmux capture-pane -t "$session" -p 2>/dev/null || true)"
  tmux kill-session -t "$session" 2>/dev/null || true
  contains "landing: a changed file opens on the file review" "$landing" "changed since your review"
  excludes "landing: it does not open on the whole-file diff" "$landing" "@@ -1,5"
else
  echo "  skip tmux not installed; the landing check needs a real terminal"
fi

echo "== result: $fail failure(s) of $total checks"
[ "$fail" -eq 0 ]
