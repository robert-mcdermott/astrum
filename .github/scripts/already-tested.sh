#!/bin/sh
# Has this exact code passed its tests already? Then it is not tested again.
#
# "This exact code" is every file of the checkout, the tests and the
# workflows among them (the roadmap and the changelog aside), and nothing
# of how it got here: see code-key.sh. A branch, the merge of that branch
# into a main that has not moved, and a tag on either are the same code,
# and one run of the suite speaks for all of them.
#
# A run of CI that passes leaves an artifact named tests-passed-<key>
# (see the last job of ci.yml), kept for thirty days. This looks for one.
#
# It says what it found, and writes for the workflow:
#   key=<the code's key>
#   run=true    the suite is to be run
#   run=false   it need not be: it has passed, or another run is doing it
# Whenever it cannot tell, it says run=true.
#
# From the workflow, in the environment:
#   GITHUB_REPOSITORY, GITHUB_EVENT_NAME, GH_TOKEN
#   PR_HEAD_REPO, PR_HEAD_REF   for a pull request: where its branch is
#   FORCE=true                  run whatever has been seen (a run asked for by hand)
set -u

key_of() { sh "$(dirname "$0")/code-key.sh" "$1" 2>/dev/null; }
key=$(key_of HEAD)
[ -n "$key" ] || exit 1

decide() {
  echo "$2"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "$2" >> "$GITHUB_STEP_SUMMARY"; fi
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    { echo "key=$key"; echo "run=$1"; } >> "$GITHUB_OUTPUT"
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
    # (A branch with a slash in its name is not one of those: `wave-*`
    # in the workflow does not reach past one.)
    */*) ;;
    main | wave-*)
      branch_key=$(key_of 'HEAD^2')
      if [ -n "$branch_key" ] && [ "$branch_key" = "$key" ]; then
        decide false "Not run here: merged, this pull request is exactly its branch (code $key), and the push to ${PR_HEAD_REF} is tested by a run of its own. Its checks are the ones to read."
      fi
      ;;
  esac
fi

# A run that passed this code before. Only one of this repository's own:
# a pull request from a fork runs the fork's workflow, which could leave
# an artifact of any name.
#
# (A run made again by hand was seen not to find the note it had left the
# time before, though a different run found it at once. So such a run
# tests again, which is what was asked of it.)
if ! passed=$(gh api "repos/${GITHUB_REPOSITORY:-}/actions/artifacts?name=tests-passed-$key&per_page=30" \
  --jq '[.artifacts[] | select(.expired == false and .workflow_run.head_repository_id == .workflow_run.repository_id)][0].workflow_run.id // empty' 2>&1); then
  decide true "GitHub could not be asked whether this code (code $key) has passed, so it is tested. What it said: $(echo "$passed" | head -n 2 | tr '\n' ' ' | cut -c1-300)"
fi
case "$passed" in
  '') decide true "This code (code $key) has not passed before: it is tested." ;;
  *[!0-9]*) decide true "GitHub's answer about this code (code $key) is not the number of a run, so it is tested." ;;
  *) decide false "Not run again: this code (code $key) passed in https://github.com/${GITHUB_REPOSITORY}/actions/runs/$passed" ;;
esac
