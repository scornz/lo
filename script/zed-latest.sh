#!/usr/bin/env bash
# Builds rozed against a newer Zed release and reports what breaks, so a Zed bump starts with a list.
# The weekly workflow (.github/workflows/weekly-zed.yml) runs it against Zed's latest release.
#
# Usage: script/zed-latest.sh [--tag <Zed tag>] [--dir <folder>]
#
#   --tag  the Zed release, such as v1.28.0. Defaults to Zed's latest release, found with `gh`.
#   --dir  a new or empty folder to work in. Defaults to a new temporary folder.
#
# It checks rozed's HEAD out into a git worktree, <folder>/rozed, and puts a scratch Zed repository
# in its third_party/zed: the release's tag with rozed's patches cherry-picked on top. The patches
# are the commits between the Zed tag rozed pins and the commit it pins (CLAUDE.md, "Zed
# submodule"). There it runs `cargo xtask sync-zed --seed-lock`, then rustfmt, a build, clippy and
# the tests, as CI does. Nothing outside <folder> changes, and nothing is pushed.
#
# Each step's output goes to <folder>/logs/. The report, a Markdown summary naming each failing step
# and its first errors, goes to <folder>/report.md and is printed at the end. Exits 0 when every
# step passes, 1 when a patch or step fails, and 2 when the script can't run (bad arguments, a
# failed fetch).
#
# Run it under `mise exec --`, so cargo finds mise's cmake. A build needs about 30 GB in <folder>;
# remove it with `git worktree remove --force <folder>/rozed`.
set -Eeuo pipefail
# Anything that fails outside the steps below is the script's own failure, not Zed's.
trap 'exit 2' ERR

usage='usage: script/zed-latest.sh [--tag <Zed tag>] [--dir <folder>]'
die() {
  echo "zed-latest: $*" >&2
  exit 2
}

tag=
dir=
while [[ $# -gt 0 ]]; do
  case $1 in
    --tag | --dir)
      [[ $# -ge 2 ]] || die "$1 needs a value"
      if [[ $1 == --tag ]]; then tag=$2; else dir=$2; fi
      shift 2
      ;;
    -h | --help)
      echo "$usage"
      exit 0
      ;;
    *)
      echo "$usage" >&2
      exit 2
      ;;
  esac
done

cd "$(dirname "$0")/.."
upstream=https://github.com/zed-industries/zed.git

# What HEAD pins: a patch branch such as rozed/v1.27.0 (or rozed/v1.27.0-2, a rewrite of the
# patches on the same release), and a commit on it.
branch=$(git config --blob HEAD:.gitmodules submodule.third_party/zed.branch) ||
  die "HEAD's .gitmodules names no branch for third_party/zed"
fork=$(git config --blob HEAD:.gitmodules submodule.third_party/zed.url)
pinned=$(git rev-parse HEAD:third_party/zed)
base=${branch#rozed/}
base=${base%-[0-9]*}

if [[ -z $tag ]]; then
  tag=$(gh api repos/zed-industries/zed/releases/latest --jq .tag_name) ||
    die "couldn't look up Zed's latest release; pass --tag"
fi

if [[ -z $dir ]]; then
  tmp=${TMPDIR:-/tmp}
  dir=$(mktemp -d "${tmp%/}/rozed-zed-latest.XXXXXX")
else
  mkdir -p "$dir"
  [[ -z $(ls -A "$dir") ]] || die "$dir isn't empty"
fi
dir=$(cd "$dir" && pwd -P)
wt=$dir/rozed
zed=$wt/third_party/zed
logs=$dir/logs
mkdir "$logs"

rozed_short=$(git rev-parse --short HEAD)
echo "zed-latest: building rozed $rozed_short against Zed $tag in $dir"
git worktree add --quiet --detach "$wt" HEAD

zgit() { git -C "$zed" -c advice.detachedHead=false "$@"; }
mkdir -p "$zed"
zgit init --quiet
zgit remote add upstream "$upstream"
zgit remote add fork "$fork"
# The pinned tag first: with it here, the fork sends only the patch commits, not Zed's history.
zgit fetch --quiet --depth 1 upstream tag "$base" || die "couldn't fetch Zed's $base"
zgit fetch --quiet fork "$pinned" || die "couldn't fetch the pinned commit $pinned from $fork"
zgit merge-base --is-ancestor "refs/tags/$base" "$pinned" ||
  die "the pinned commit $pinned isn't on top of Zed's $base"
zgit fetch --quiet --depth 1 upstream tag "$tag" || die "couldn't fetch Zed's $tag"
zgit checkout --quiet --detach "refs/tags/$tag^{commit}"
patches=$(zgit rev-list --reverse "refs/tags/$base..$pinned")
pinned_short=$(zgit rev-parse --short "$pinned")

# The report: a table row per step, and a section per failure.
rows=
details=
broken=0
row() { rows+="| $1 | $2 |"$'\n'; }

# Replay the patches. A patch that no longer applies is a breakage in itself; one that's now empty
# is already in Zed and can be dropped.
total=0
failed=0
patch_list=
for sha in $patches; do
  total=$((total + 1))
  line="$total. \`$(zgit rev-parse --short "$sha")\` $(zgit log -1 --format=%s "$sha")"
  echo "zed-latest: cherry-picking $(zgit log -1 --format='%h %s' "$sha")"
  status=0
  out=$(zgit -c user.name=zed-latest -c user.email=zed-latest@localhost -c commit.gpgsign=false \
    cherry-pick "$sha" 2>&1) || status=$?
  printf '%s\n' "$out" | tee -a "$logs/patches.log"
  if [[ $status -eq 0 ]]; then
    patch_list+="$line: applies"$'\n'
  elif [[ -z $(zgit diff --name-only --diff-filter=U) ]] && zgit diff --cached --quiet; then
    zgit cherry-pick --skip
    patch_list+="$line: already in Zed, so the patch can go"$'\n'
  else
    failed=$((failed + 1))
    conflicts=$(printf '%s\n' "$out" | grep '^CONFLICT' || printf '%s\n' "$out" | tail -n 5)
    zgit cherry-pick --abort
    patch_list+="$line: **doesn't apply**"$'\n\n'"   \`\`\`text"$'\n'
    patch_list+="$(printf '%s\n' "$conflicts" | sed 's/^/   /')"$'\n'"   \`\`\`"$'\n\n'
  fi
done
if [[ $failed -eq 0 ]]; then
  row "Replay rozed's $total patches" passes
elif [[ $failed -eq 1 ]]; then
  row "Replay rozed's $total patches" "**fails**: 1 doesn't apply"
  broken=1
else
  row "Replay rozed's $total patches" "**fails**: $failed don't apply"
  broken=1
fi

# Only enough to compile and test, as in CI: no incremental state or debug info. Values already set
# are kept.
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
export CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-0}
export CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG=${CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG:-0}

# The first errors in a step's log: rustc's error diagnostics, each once (a crate's lib and its
# tests report the same ones), then any failed tests, else the log's end. Paths are shown relative
# to the worktree.
first_errors() {
  local log=$logs/$1.log text
  text=$(sed $'s/\x1b\\[[0-9;]*[A-Za-z]//g' "$log" | awk -v max=8 '
    function flush() {
      if (block && !(key in seen)) {
        seen[key] = 1
        if (++distinct <= max) printf "%s\n\n", text
      }
      block = 0
    }
    /^(error|warning)/ { flush() }
    /^error(\[E[0-9]+\])?: / && !/^error: (could not compile|aborting due to|test failed|[0-9]+ targets? failed)/ {
      block = 1; located = 0; key = $0; text = $0; lines = 1; next
    }
    block && /^$/ { flush(); next }
    # The message and where it points identify an error.
    block && !located && /^ *--> / { key = key " " $0; located = 1 }
    block && ++lines <= 30 { text = text "\n" $0 }
    END {
      flush()
      if (distinct > max) printf "... and %d more errors.\n", distinct - max
    }
  ')
  if [[ -z $text && $1 == fmt ]]; then
    text=$(grep '^Diff in ' "$log" | head -n 30 || true)
  fi
  if [[ -z $text && $1 == test ]]; then
    text=$(
      grep -E '^test .* \.\.\. FAILED$' "$log" | head -n 30 || true
      echo
      awk '/^---- .* stdout ----$/ { n++ } n == 1 && /^failures:$/ { exit } n == 1 && ++lines <= 40 { print } n > 1 { exit }' "$log"
    )
  fi
  if [[ -z ${text//[[:space:]]/} ]]; then
    text=$(tail -n 30 "$log")
  fi
  printf '%s\n' "$text" | sed -e "s#$wt/##g"
}

# step <name> <command...>: runs the command in the worktree, logging its output to logs/<name>.log
# and the terminal, and adds a row for it to the report. Returns the command's status.
step() {
  local name=$1 label status=0
  shift
  label=$*
  echo "zed-latest: running $label"
  (cd "$wt" && "$@") 2>&1 | tee "$logs/$name.log" || status=$?
  if [[ $status -eq 0 ]]; then
    row "\`$label\`" passes
    return 0
  fi
  broken=1
  row "\`$label\`" "**fails**"
  details+=$'\n'"### \`$label\`: first errors"$'\n\n'"\`\`\`text"$'\n'"$(first_errors "$name")"$'\n'"\`\`\`"$'\n'
  return "$status"
}
skip() {
  local reason=$1 label
  shift
  for label in "$@"; do row "\`$label\`" "skipped: $reason"; done
}

channel() { sed -n 's/^channel = "\(.*\)"$/\1/p' "$wt/rust-toolchain.toml"; }
rust_before=$(channel)

sync=(cargo xtask sync-zed --seed-lock)
fmt=(cargo fmt --check)
build=(cargo build --locked --workspace --all-targets)
clippy=(cargo clippy --locked --workspace --all-targets -- -D warnings)
tests=(cargo test --locked --workspace --no-fail-fast)
if [[ $failed -gt 0 ]]; then
  skip "a patch doesn't apply" "${sync[*]}" "${fmt[*]}" "${build[*]}" "${clippy[*]}" "${tests[*]}"
elif ! step sync-zed "${sync[@]}"; then
  skip "sync-zed fails" "${fmt[*]}" "${build[*]}" "${clippy[*]}" "${tests[*]}"
else
  # Zed's release may pin a newer Rust, which sync-zed wrote to rust-toolchain.toml.
  if command -v rustup >/dev/null; then (cd "$wt" && rustup toolchain install); fi
  step fmt "${fmt[@]}" || true
  # A build first, so compile errors are reported once rather than by both clippy and the tests.
  if step build "${build[@]}"; then
    step clippy "${clippy[@]}" || true
    step test "${tests[@]}" || true
  else
    skip "the build fails" "${clippy[*]}" "${tests[*]}"
  fi
fi
rust_after=$(channel)

report=$dir/report.md
{
  echo "## rozed against Zed $tag"
  echo
  if [[ $broken -eq 0 ]]; then
    echo "rozed builds and passes its checks against Zed $tag."
  else
    echo "**Zed $tag breaks rozed.**"
  fi
  echo
  echo "Zed [$tag](https://github.com/zed-industries/zed/releases/tag/$tag) with rozed's patches" \
    "from \`$branch\` (\`$pinned_short\`), at rozed \`$rozed_short\`."
  if [[ -n $rust_after && $rust_after != "$rust_before" ]]; then
    echo "Zed $tag moves Rust from $rust_before to $rust_after."
  fi
  echo
  echo "| Step | Result |"
  echo "| --- | --- |"
  printf '%s' "$rows"
  if [[ -n $patch_list ]]; then
    printf '\n### Patches\n\n%s' "$patch_list"
  fi
  printf '%s' "$details"
} >"$report"

echo
cat "$report"
echo
echo "zed-latest: the worktree, logs and report are in $dir."
echo "zed-latest: remove them with \`git worktree remove --force $wt && rm -rf $dir\`."
exit "$broken"
