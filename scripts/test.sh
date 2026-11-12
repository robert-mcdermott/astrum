#!/bin/sh
# Run the whole suite, unless this exact code has passed it already.
#
#   scripts/test.sh            run `cargo test --locked`, or say why there is no need
#   scripts/test.sh --again    run it whatever has passed before
#   scripts/test.sh -- ARGS    anything else is a part of the suite: always run, never remembered
#
# "This exact code" is every file as it is in the working directory now,
# committed or not (ignored files aside), with the compiler that would
# build it. A run that passes is noted in target/tests-passed; a run of CI
# that passed the same code counts as well, where `gh` can be asked.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

again=
if [ "${1:-}" = --again ]; then again=1; shift; fi
if [ "${1:-}" = -- ]; then shift; exec cargo test --locked "$@"; fi
if [ $# -gt 0 ]; then echo "usage: scripts/test.sh [--again] [-- cargo test arguments]" >&2; exit 2; fi

# The working directory as a tree, without touching the index.
index=$(mktemp) || exit 1
trap 'rm -f "$index"' EXIT
cp "$(git rev-parse --git-path index)" "$index" 2>/dev/null
tree=$(GIT_INDEX_FILE="$index" sh -c 'git add -A && git write-tree') || exit 1
with=$(rustc --version 2>/dev/null || echo unknown)
noted=target/tests-passed

if [ -z "$again" ]; then
  if grep -qsxF "$tree $with" "$noted"; then
    echo "Not run again: this code (tree $tree) has passed here, with $with."
    echo "(scripts/test.sh --again runs it all the same.)"
    exit 0
  fi
  # Committed and pushed as it stands? Then CI may have passed it.
  if [ "$tree" = "$(git rev-parse 'HEAD^{tree}' 2>/dev/null)" ] && command -v gh >/dev/null 2>&1; then
    run=$(gh api "repos/{owner}/{repo}/actions/artifacts?name=tests-passed-$tree&per_page=30" \
      --jq '[.artifacts[] | select(.expired == false and .workflow_run.head_repository_id == .workflow_run.repository_id)][0].workflow_run.id // empty' 2>/dev/null) || run=
    case "$run" in
      '' | *[!0-9]*) ;;
      *)
        echo "Not run again: this code (tree $tree) passed in CI, on every system there is a release for:"
        gh api "repos/{owner}/{repo}/actions/runs/$run" --jq .html_url 2>/dev/null
        echo "(scripts/test.sh --again runs it here all the same.)"
        exit 0
        ;;
    esac
  fi
fi

cargo test --locked || exit $?
mkdir -p target && echo "$tree $with" >> "$noted"
echo "Passed, and noted: this code (tree $tree) is not run again until something changes."
