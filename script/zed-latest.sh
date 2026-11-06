#!/usr/bin/env bash
# Builds rozed against a newer Zed release and reports what breaks, so a Zed bump starts with a list.
# The weekly workflow (.github/workflows/weekly-zed.yml) runs it against Zed's latest release.
#
# Usage: script/zed-latest.sh [--tag <Zed tag>] [--skip-pinned] [--dir <folder>]
#
#   --tag          the Zed release, such as v1.28.0 or v1.28.0-pre. Defaults to Zed's latest
#                  release, found with `gh`.
#   --skip-pinned  if the release is the one rozed pins, build nothing and say so: CI already builds
#                  it. On when --tag isn't given.
#   --dir          a new or empty folder to work in. Defaults to a new temporary folder.
#
# It checks rozed's HEAD out into a git worktree, <folder>/rozed, and puts a scratch Zed repository
# in its third_party/zed: the release's tag with rozed's patches cherry-picked on top. The patches
# are the commits between the Zed tag rozed pins and the commit it pins (CLAUDE.md, "Zed
# submodule"). In the worktree it runs `cargo xtask sync-zed --seed-lock`, `rustup toolchain
# install` and `cargo fetch`, then `cargo fmt --check`, `cargo build`, `cargo clippy` and `cargo
# test`, building into the worktree's own target/.
#
# Each step's output goes to <folder>/logs/. The report, a Markdown summary naming each failing step
# and its first errors, goes to <folder>/report.md and is printed at the end. Exits 0 when every
# step passes, 1 when a patch or step fails, and 2 when the script can't run: bad arguments, a
# failed download, a full disk. On exit 2 it removes the worktree and keeps the logs.
#
# Outside <folder>, it registers the worktree in this repository's .git, fills cargo's caches in
# ~/.cargo and, when the release pins a newer Rust, installs that toolchain with rustup in ~/.rustup
# (about 1.5 GB, not removed). It pushes nothing.
#
# Run it under `mise exec --`, so cargo finds mise's cmake. A build needs about 6 GB in <folder>;
# remove it with `git worktree remove --force <folder>/rozed && rm -rf <folder>`.
set -Eeuo pipefail
# Anything that fails outside the steps below is the script's own failure, not Zed's.
trap 'exit 2' ERR

usage='usage: script/zed-latest.sh [--tag <Zed tag>] [--skip-pinned] [--dir <folder>]'
die() {
  echo "zed-latest: $*" >&2
  exit 2
}

tag=
dir=
skip_pinned=0
while [[ $# -gt 0 ]]; do
  case $1 in
    --tag | --dir)
      [[ $# -ge 2 ]] || die "$1 needs a value"
      if [[ $1 == --tag ]]; then tag=$2; else dir=$2; fi
      shift 2
      ;;
    --skip-pinned)
      skip_pinned=1
      shift
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

# The folder, resolved before leaving the caller's directory, so a relative one is relative to it.
made_dir=0
if [[ -z $dir ]]; then
  tmp=${TMPDIR:-/tmp}
  dir=$(mktemp -d "${tmp%/}/rozed-zed-latest.XXXXXX")
  made_dir=1
else
  [[ -e $dir ]] || made_dir=1
  mkdir -p "$dir"
  [[ -z $(ls -A "$dir") ]] || die "$dir isn't empty"
fi
dir=$(cd "$dir" && pwd -P)
wt=$dir/rozed
zed=$wt/third_party/zed
logs=$dir/logs
report=$dir/report.md

root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"

# On exit 2 there's no report, only logs: remove the worktree, and the folder if the script made it
# and nothing was logged.
added=0
# shellcheck disable=SC2329 # the EXIT trap runs it
cleanup() {
  [[ $? -eq 2 ]] || return 0
  if [[ $added -eq 1 ]]; then
    echo "zed-latest: removing the worktree $wt" >&2
    git worktree remove --force "$wt" >&2 || true
    git worktree prune || true
  fi
  rmdir "$logs" 2>/dev/null || true
  if [[ $made_dir -eq 1 ]]; then rmdir "$dir" 2>/dev/null || true; fi
  if [[ -d $logs ]]; then echo "zed-latest: the logs are in $logs" >&2; fi
}
trap cleanup EXIT

# What HEAD pins: a patch branch such as rozed/v1.27.0 (or rozed/v1.27.0-2, a rewrite of the
# patches on the same release), and a commit on it.
branch=$(git config --blob HEAD:.gitmodules submodule.third_party/zed.branch) ||
  die "HEAD's .gitmodules names no branch for third_party/zed"
fork=$(git config --blob HEAD:.gitmodules submodule.third_party/zed.url)
pinned=$(git rev-parse HEAD:third_party/zed)
base=${branch#rozed/}
base=${base%-[0-9]*}
upstream=https://github.com/zed-industries/zed.git

if [[ -z $tag ]]; then
  tag=$(gh api repos/zed-industries/zed/releases/latest --jq .tag_name) ||
    die "couldn't look up Zed's latest release; pass --tag"
  skip_pinned=1
fi
# Zed's release tags look like v1.28.0 or v1.28.0-pre. Checking also keeps a tag from reading as an
# option to git.
tag_pattern='^v[0-9]+\.[0-9]+\.[0-9]+(-pre)?$'
[[ $tag =~ $tag_pattern ]] || die "$tag isn't a Zed release tag, such as v1.28.0 or v1.28.0-pre"

rozed_short=$(git rev-parse --short HEAD)
if [[ $skip_pinned -eq 1 && $tag == "$base" ]]; then
  {
    echo "## rozed against Zed $tag"
    echo
    echo "rozed already builds against Zed $tag, the release it pins, at rozed \`$rozed_short\`." \
      "CI checks that build on every pull request, so there's nothing newer to try."
  } >"$report"
  cat "$report"
  exit 0
fi

mkdir "$logs"
echo "zed-latest: building rozed $rozed_short against Zed $tag in $dir"
git worktree add --quiet --detach "$wt" HEAD
added=1

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
# Patch branches are a line of single commits; a merge in one can't be cherry-picked.
merges=$(zgit rev-list --merges "refs/tags/$base..$pinned")
patches=$(zgit rev-list --reverse --no-merges "refs/tags/$base..$pinned")
pinned_short=$(zgit rev-parse --short "$pinned")

channel() { sed -n 's/^channel = "\(.*\)"$/\1/p'; }
rust_rozed=$(git show HEAD:rust-toolchain.toml | channel)
rust_zed=
if [[ -f $zed/rust-toolchain.toml ]]; then rust_zed=$(channel <"$zed/rust-toolchain.toml"); fi

# block <indent> <text>: the text as a Markdown code block that's safe to post in an issue. Its fence
# is longer than any run of backticks in the text, so the text can't close it; an @ is followed by a
# zero-width joiner, so it can't mention anyone; and lines are cut to 400 characters.
block() {
  printf '%s\n' "$2" | perl -CSD -e '
    my $indent = shift;
    my @lines = map { chomp; $_ } <STDIN>;
    my $longest = 2;
    for (@lines) {
      $_ = substr($_, 0, 400) . " [cut]" if length > 400;
      s/\@(?!\x{200D})/\@\x{200D}/g;
      while (/(`+)/g) { $longest = length $1 if length $1 > $longest }
    }
    my $fence = "`" x ($longest + 1);
    print "$indent${fence}text\n", (map { "$indent$_\n" } @lines), "$indent$fence\n";
  ' "$1"
}

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
    patch_list+="$line: **doesn't apply**"$'\n\n'"$(block '   ' "$conflicts")"$'\n\n'
  fi
done
for sha in $merges; do
  echo "zed-latest: skipping merge commit $(zgit log -1 --format='%h %s' "$sha")"
  patch_list+=$'\n'"Skipped merge commit \`$(zgit rev-parse --short "$sha")\`: a patch branch should"
  patch_list+=" be a line of single commits."$'\n'
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
# are kept, except the target folder, which is always the worktree's.
export CARGO_TARGET_DIR=$wt/target
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
export CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-0}
export CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG=${CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG:-0}

# A log without terminal colours.
plain() { LC_ALL=C sed $'s/\x1b\\[[0-9;]*[A-Za-z]//g' "$1"; }

# The first errors in a step's log, with paths relative to the worktree. For the tests: the failing
# tests by name, then the first one's output. Otherwise rustc's error diagnostics, each once (a
# crate's lib and its tests report the same ones), then rustfmt's diffs, else the log's end.
first_errors() {
  local name=$1 log=$logs/$1.log text=
  if [[ $name == test ]]; then
    text=$(plain "$log" | LC_ALL=C grep -E '^test .* \.\.\. FAILED$|^test result: FAILED' |
      head -n 40 || true)
    if [[ -n $text ]]; then
      text+=$'\n\n'$(plain "$log" | LC_ALL=C awk '
        /^---- .* stdout ----$/ { n++ }
        n > 1 || (n == 1 && /^failures:$/) { exit }
        n == 1 && ++lines <= 40 { print }
      ' || true)
    fi
  fi
  if [[ -z $text ]]; then
    text=$(plain "$log" | LC_ALL=C awk -v max=8 '
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
    ' || true)
  fi
  if [[ -z $text && $name == fmt ]]; then
    text=$(plain "$log" | LC_ALL=C grep '^Diff in ' | head -n 30 || true)
  fi
  if [[ -z ${text//[[:space:]]/} ]]; then
    text=$(plain "$log" | tail -n 30)
  fi
  # The worktree's path as literal text, not a pattern.
  printf '%s\n' "$text" | perl -pe 'BEGIN { $prefix = shift } s/\Q$prefix\E//g' "$wt/"
}

# run <name> <command...>: runs the command in the worktree, its output going to the terminal and to
# logs/<name>.log. Returns the command's status; exits 2 if the log can't be written.
run() {
  local name=$1 status=0 tee_status=0
  shift
  echo "zed-latest: running $*"
  if ! (cd "$wt" && "$@") 2>&1 | tee "$logs/$name.log"; then
    status=${PIPESTATUS[0]} tee_status=${PIPESTATUS[1]}
  fi
  [[ $tee_status -eq 0 ]] || die "couldn't write $logs/$name.log"
  return "$status"
}

# outside_failure <name> <label>: exits 2 when a failed step's log shows that the machine or the
# network failed it, not Zed: a full disk in any step, or a failed download in a step that may
# download. Lines quoting source code are skipped, since Zed's own code mentions such failures, and
# so are warnings, since cargo warns of a download it then retries successfully.
outside_failure() {
  local name=$1 label=$2 pattern='No space left on device|Disk quota exceeded' line
  case $name in
    sync-zed | build | clippy)
      pattern+="|Could not resolve host|Couldn't resolve host|failed to resolve address"
      pattern+="|Couldn't connect to server|Connection refused|Connection reset by peer"
      pattern+="|Operation timed out|Timeout was reached|SSL connect error"
      pattern+="|Failure when receiving data|failed to download from|unable to access 'https?://"
      ;;
  esac
  line=$(plain "$logs/$name.log" | LC_ALL=C grep -vE '^ *[0-9]* *\||^warning: ' |
    LC_ALL=C grep -m 1 -E "$pattern" || true)
  [[ -z $line ]] || die "$label failed outside Zed and rozed: $line (see $logs/$name.log)"
}

# step <name> <command...>: runs the command and adds a row for it to the report. Returns the
# command's status.
step() {
  local name=$1 label status=0
  shift
  label=$*
  run "$name" "$@" || status=$?
  if [[ $status -eq 0 ]]; then
    row "\`$label\`" passes
    return 0
  fi
  outside_failure "$name" "$label"
  broken=1
  row "\`$label\`" "**fails**"
  details+=$'\n'"### \`$label\`: first errors"$'\n\n'"$(block '' "$(first_errors "$name")")"$'\n'
  return "$status"
}
skip() {
  local reason=$1 label
  shift
  for label in "$@"; do row "\`$label\`" "skipped: $reason"; done
}

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
  # Downloads, so a failure here is the machine's or the network's, not Zed's. sync-zed wrote Zed's
  # toolchain to rust-toolchain.toml.
  if command -v rustup >/dev/null; then
    run toolchain rustup toolchain install || die "couldn't install Zed's Rust; see $logs/toolchain.log"
  fi
  host=$(cd "$wt" && rustc -vV | sed -n 's/^host: //p')
  run fetch cargo fetch --locked --target "$host" ||
    die "couldn't download rozed's dependencies; see $logs/fetch.log"

  step fmt "${fmt[@]}" || true
  # A build first, so compile errors are reported once rather than by both clippy and the tests.
  if step build "${build[@]}"; then
    step clippy "${clippy[@]}" || true
    step test "${tests[@]}" || true
  else
    skip "the build fails" "${clippy[*]}" "${tests[*]}"
  fi
fi

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
  if [[ -n $rust_zed && $rust_zed != "$rust_rozed" ]]; then
    echo "Zed $tag moves Rust from $rust_rozed to $rust_zed."
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
printf 'zed-latest: remove them with: git worktree remove --force %q && rm -rf %q\n' "$wt" "$dir"
exit "$broken"
