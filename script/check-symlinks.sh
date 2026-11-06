#!/bin/sh
# Checks the links that give every agent one set of rules:
# - every CLAUDE.md has an AGENTS.md -> CLAUDE.md symlink beside it, for Codex
# - .agents/skills is a symlink to ../.claude/skills, the folder Codex reads skills from
# Skips hidden folders, dependencies, build output and third_party/ (Zed has its own layout).
# Usage: script/check-symlinks.sh [repo root], defaulting to this script's repo.
root=${1:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
cd "$root" || exit 1
status=0
fail() {
  echo "$1" >&2
  status=1
}

# One path per line, so paths with spaces survive the loop.
IFS='
'
for claude in $(find . \( -name '.?*' -o -name node_modules -o -name target -o -name third_party \) -prune \
  -o -name CLAUDE.md -print | sort); do
  agents=$(dirname "$claude")/AGENTS.md
  agents=${agents#./}
  # The CLAUDE.md beside it exists, so a link to it resolves.
  if [ ! -L "$agents" ] || [ "$(readlink "$agents")" != CLAUDE.md ]; then
    fail "$agents: must be a symlink to CLAUDE.md (ln -s CLAUDE.md $agents)"
  fi
done

if [ ! -L .agents/skills ] || [ "$(readlink .agents/skills)" != ../.claude/skills ]; then
  fail ".agents/skills: must be a symlink to ../.claude/skills (ln -s ../.claude/skills .agents/skills)"
elif [ ! -d .agents/skills ]; then
  fail ".agents/skills: doesn't resolve to a folder"
fi

[ "$status" -eq 0 ] && echo "Agent symlinks OK."
exit "$status"
