#!/usr/bin/env bash
# Hands-on launcher for the round-2 review features. It builds a scenario in a
# scratch data dir, prints what to press and what to look for, then opens the TUI
# so you can drive it yourself.
#
#   bash scripts/try-review.sh list                 # show the cases
#   bash scripts/try-review.sh fresh                # unchanged file, no overlay
#   bash scripts/try-review.sh round2               # changed after review (overlay + verbs)
#   bash scripts/try-review.sh patch                # a .patch review
#   bash scripts/try-review.sh folder               # folder mode with one changed file
#   bash scripts/try-review.sh reset                # delete the scratch dir
#   bash scripts/try-review.sh --setup-only round2  # prepare but do not launch
#
# State persists under $PLANNOTATOR_TRY_DIR (default: ${TMPDIR:-/tmp}/plannotator-try),
# so relaunching a case keeps the same annotations, baseline and reviewed sidecars.
# Nothing touches ~/.plannotator. The printed keystrokes are a summary; the full
# walkthrough is diff-review-manual-tests.md.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
bin="${PLANNOTATOR_TUI_BIN:-$root/target/debug/plannotator-tui}"
dir="${PLANNOTATOR_TRY_DIR:-${TMPDIR:-/tmp}/plannotator-try}"
dir="${dir%/}"
export PLANNOTATOR_DATA_DIR="$dir/data"

setup_only=0
if [ "${1:-}" = "--setup-only" ]; then setup_only=1; shift; fi
case_name="${1:-list}"

list() {
  cat <<'TXT'
cases:
  fresh    a file with no review: no overlay, plain editing
  round2   a reviewed file the "agent" then changed: overlay, changes view, region verbs
  patch    a unified diff opened as an annotatable document
  folder   folder mode; b.md changed after review
  peek     show the baseline / reviewed sidecars the verbs move
  reset    delete the scratch dir and start clean
TXT
}

launch() { # launch <path>
  if [ "$setup_only" = 1 ]; then
    echo
    echo "launch with: $bin $1"
    return
  fi
  exec "$bin" "$1"
}

print_keys() { printf '\nkeys: %s\n' "$1"; }

case "$case_name" in
  list|"")
    list
    ;;

  reset)
    rm -rf "$dir"
    echo "removed $dir"
    ;;

  fresh)
    mkdir -p "$dir/data"
    printf '# Fresh file\n\nA paragraph with no review behind it.\n\nAnother paragraph.\n' > "$dir/fresh.md"
    echo "== fresh: no review behind the file"
    echo "Open: $dir/fresh.md"
    print_keys "j/k (move blocks) · v then hjkl (select) · c (comment) · E (send) · q (quit)"
    echo "expect: no +N −M chip, no gutter marks, no changes view"
    launch "$dir/fresh.md"
    ;;

  round2)
    mkdir -p "$dir/data"
    printf '# Plan\n\nAlpha line.\n\nBeta line.\n' > "$dir/plan.md"
    "$bin" --annotate "$dir/plan.md" "Alpha line." note "please expand this" >/dev/null
    printf '# Plan\n\nAlpha line edited by the agent.\n\nBeta line edited too.\n' > "$dir/plan.md"
    echo "== round2: the agent changed two blocks after your review"
    echo "Open: $dir/plan.md  (opens on the file review; press i for the whole-file diff)"
    print_keys "j/k move · a accept block · A accept all · D revert block · X revert all · U un-accept · i/Esc diff · c+E comment+send"
    echo "expect: footer '+2 −2 changed since your review'; gutter mark fg green on added"
    echo "        rows, red where baseline text was removed"
    launch "$dir/plan.md"
    ;;

  patch)
    mkdir -p "$dir/data"
    cat > "$dir/fix.patch" <<'PATCH'
diff --git a/plan.md b/plan.md
--- a/plan.md
+++ b/plan.md
@@ -1,5 +1,5 @@
 # Plan
 
-Alpha line.
+Alpha line edited.
 
-Beta line.
+Beta line edited.
PATCH
    echo "== patch: a unified diff reviewed as a document"
    echo "Open: $dir/fix.patch"
    print_keys "j/k (move) · v then hjkl (select a line) · c (comment) · r (reload) · E (send) · q"
    echo "expect: file header and each hunk are blocks; body lines keep their +/-/space prefixes"
    echo "note:   nothing is persisted -- regenerating the patch is a new, empty review"
    launch "$dir/fix.patch"
    ;;

  folder)
    mkdir -p "$dir/data" "$dir/docs"
    printf '# A\n\nAlpha text.\n' > "$dir/docs/a.md"
    printf '# B\n\nBeta text.\n' > "$dir/docs/b.md"
    "$bin" --annotate "$dir/docs/a.md" "Alpha" note "a note" >/dev/null
    "$bin" --annotate "$dir/docs/b.md" "Beta" note "a note" >/dev/null
    printf '# B\n\nBeta text, revised.\n' > "$dir/docs/b.md"
    echo "== folder: two reviewed files, b.md changed"
    echo "Open: $dir/docs"
    print_keys "j/k then Enter (tree) · t (toggle tree) · Tab (cycle focus) · E (send) · q"
    echo "expect: tree counts per file; b.md shows '+1 −1 changed since your review' and marks,"
    echo "        a.md shows neither; a folder send counts the real records"
    launch "$dir/docs"
    ;;

  peek)
    found=0
    while IFS= read -r baseline; do
      [ -n "$baseline" ] || continue
      found=1
      record="$(dirname "$baseline")"
      echo "== ${record#"$dir"/}"
      echo "--- baseline.md (the reference the file is marked against)"
      cat "$baseline"
      echo "--- reviewed.md (the round's original, moved only by an accept-all)"
      cat "$record/reviewed.md" 2>/dev/null || echo "(none)"
      echo
    done < <(find "$dir" -name baseline.md 2>/dev/null)
    [ "$found" = 1 ] || echo "no review state yet; run the round2 case first"
    ;;

  *)
    echo "unknown case: $case_name" >&2
    list >&2
    exit 2
    ;;
esac
