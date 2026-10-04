#!/bin/sh
# Copies .env and .env.* from the project's main worktree into the new
# worktree, $CRYSTAL_WORKTREE, each that isn't there already: they're kept
# out of git, so a new worktree has none.
set -eu
project=${CRYSTAL_PROJECT:-}
worktree=${CRYSTAL_WORKTREE:-}
if [ -z "$project" ] || [ -z "$worktree" ] || [ "$project" = "$worktree" ]; then
  exit 0
fi
for file in "$project"/.env "$project"/.env.*; do
  [ -f "$file" ] || continue
  name=$(basename "$file")
  [ -e "$worktree/$name" ] && continue
  cp "$file" "$worktree/$name"
  echo "copied $name into $worktree"
done
