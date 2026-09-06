#!/bin/sh
# Point this repository's hooks at the ones kept in it.
#
# Git does not commit .git/hooks, so a hook everybody has is a hook everybody
# had to be told to install. core.hooksPath is the one line that fixes that,
# and it is per repository rather than global, so nothing here changes how git
# behaves anywhere else.

set -e
cd "$(dirname "$0")/.."

chmod +x .githooks/*
git config core.hooksPath .githooks

echo "hooks: core.hooksPath -> .githooks"
echo "  pre-commit  formats the Rust you are committing"
echo "skip one with: git commit --no-verify"
