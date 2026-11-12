#!/bin/sh
# Has this exact code passed its tests already? Then it is not tested again.
#
# "This exact code" is the tree: every file of the checkout, the tests and
# the workflows among them, and nothing of how it got here. A branch, the
# merge of that branch into a main that has not moved, and a tag on either
# are the same tree, and one run of the suite speaks for all of them.
#
# A run of CI that passes leaves an artifact named tests-passed-<tree>
# (see the last job of ci.yml), kept for thirty days. This looks for one.
#
# It says what it found, and writes for the workflow:
#   tree=<the tree>
#   run=true    the suite is to be run
#   run=false   it need not be: it has passed, or another run is doing it
# Whenever it cannot tell, it says run=true.
#
# From the workflow, in the environment:
#   GITHUB_REPOSITORY, GITHUB_EVENT_NAME, GH_TOKEN
#   PR_HEAD_REPO, PR_HEAD_REF   for a pull request: where its branch is
#   FORCE=true                  run whatever has been seen (a run asked for by hand)
set -u

tree=$(git rev-parse 'HEAD^{tree}') || exit 1

decide() {
  echo "$2"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "$2" >> "$GITHUB_STEP_SUMMARY"; fi
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    { echo "tree=$tree"; echo "run=$1"; } >> "$GITHUB_OUTPUT"
  fi
  exit 0
}

if [ "${FORCE:-}" = true ]; then
  decide true "Asked for by hand: everything is run, whatever has passed before."
fi

# A pull request from a branch of this repository whose pushes are tested
# anyway. What a pull request's run tests is the branch merged into its
# base; while the base has not moved, that is the branch itself, file for
# file, and the push that brought the branch here is testing it right now.
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ] && [ -n "${PR_HEAD_REPO:-}" ] && [ "${PR_HEAD_REPO:-}" = "${GITHUB_REPOSITORY:-}" ]; then
  case "${PR_HEAD_REF:-}" in
    main | wave-*)
      branch_tree=$(git rev-parse --verify --quiet 'HEAD^2^{tree}' || true)
      if [ -n "$branch_tree" ] && [ "$branch_tree" = "$tree" ]; then
        decide false "Not run here: merged, this pull request is exactly its branch (tree $tree), and the push to ${PR_HEAD_REF} is tested by a run of its own. Its checks are the ones to read."
      fi
      ;;
  esac
fi

# A run that passed this tree before. Only one of this repository's own:
# a pull request from a fork runs the fork's workflow, which could leave
# an artifact of any name.
passed=$(gh api "repos/${GITHUB_REPOSITORY:-}/actions/artifacts?name=tests-passed-$tree&per_page=30" \
  --jq '[.artifacts[] | select(.expired == false and .workflow_run.head_repository_id == .workflow_run.repository_id)][0].workflow_run.id // empty' 2>/dev/null) || passed=
case "$passed" in
  '' | *[!0-9]*) ;;
  *) decide false "Not run again: this code (tree $tree) passed in https://github.com/${GITHUB_REPOSITORY}/actions/runs/$passed" ;;
esac

decide true "This code (tree $tree) has not passed before: it is tested."
