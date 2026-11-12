#!/bin/sh
# The name a state of the code goes by: the same for the same files,
# whatever commit, branch or tag they are reached by.
#
#   code-key.sh            for what is checked out (HEAD)
#   code-key.sh <tree-ish> for a commit or a tree
#
# It is made from every file's path and contents, except the files below,
# which nothing that is built or tested reads: a note in the roadmap is
# not a change to the code, and does not make it untested. (The release
# workflow reads the changelog, in a step of its own that always runs.)
set -u
tab=$(printf '\t')
git ls-tree -r "${1:-HEAD}" | grep -v -E "${tab}(ROADMAP\\.md|CHANGELOG\\.md)\$" | git hash-object --stdin
