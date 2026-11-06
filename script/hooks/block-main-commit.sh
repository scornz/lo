#!/bin/sh
# pre-commit: refuse commits made directly on main.
branch=$(git symbolic-ref --short -q HEAD)
if [ "$branch" = "main" ]; then
  echo "Refusing to commit on 'main'. Create a branch first (git switch -c <type>/<topic>); all changes land via pull requests." >&2
  exit 1
fi
