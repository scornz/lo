#!/usr/bin/env bash
# Checks that the Zed crates compiled into rozed match script/zed-crates.txt, so a change in what
# rozed pulls in from Zed shows up in review. With --update, rewrites the list instead.
#
# "Compiled in" means what `cargo build -p lo` compiles for the host: the crates reachable from
# rozed through normal dependencies, with the features a build of the binary turns on. A Zed crate
# is one whose manifest sits under third_party/zed.
#
# This uses `cargo tree` rather than `cargo metadata`: metadata's dependency graph merges in the
# features rozed's dev-dependencies turn on (`test-support`), which pull in crates the binary never
# builds, such as `util_macros` and `perf`. `cargo tree -e normal` resolves features as the build
# does.
set -euo pipefail

cd "$(dirname "$0")/.."
list=script/zed-crates.txt

host=$(rustc -vV | sed -n 's/^host: //p')
actual=$(
  cargo tree --locked --package lo --edges normal --target "$host" --prefix none --format '{p}' |
    grep -F "($PWD/third_party/zed/" |
    awk '{ print $1 }' |
    sort -u
)

if [[ ${1:-} == --update ]]; then
  printf '%s\n' "$actual" > "$list"
  echo "Wrote $(wc -l < "$list" | tr -d ' ') Zed crates to $list."
  exit 0
fi

if ! diff_output=$(diff -u "$list" <(printf '%s\n' "$actual")); then
  echo "The Zed crates compiled into rozed changed (- in $list, + now):"
  printf '%s\n' "$diff_output" | tail -n +3
  echo "If that's intended, run script/check-zed-crates.sh --update, update docs/spike.md and commit both."
  exit 1
fi
echo "rozed compiles in the $(wc -l < "$list" | tr -d ' ') Zed crates listed in $list."
