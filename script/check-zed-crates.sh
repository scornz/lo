#!/usr/bin/env bash
# Checks that the Zed crates compiled into rozed match script/zed-crates.txt, so a change in what
# rozed pulls in from Zed shows up in review. With --update, rewrites the list instead.
#
# "Compiled in" means reachable from the rozed package through normal dependencies on the host
# platform, per `cargo metadata`: the crates linked into the binary. Build and dev dependencies
# don't count. A Zed crate is one whose manifest sits under third_party/zed.
set -euo pipefail

cd "$(dirname "$0")/.."
list=script/zed-crates.txt

host=$(rustc -vV | sed -n 's/^host: //p')
actual=$(
  cargo metadata --format-version 1 --locked --filter-platform "$host" |
    jq -r --arg zed "$PWD/third_party/zed/" '
      (.packages | map({key: .id, value: .}) | from_entries) as $packages
      | (.resolve.nodes | map({key: .id, value: .}) | from_entries) as $nodes
      | (.packages[] | select(.name == "rozed" and .source == null) | .id) as $root
      | {seen: {($root): true}, queue: [$root]}
      | until(.queue | length == 0;
          .queue[0] as $id
          | .queue |= .[1:]
          | reduce ($nodes[$id].deps[]
                    | select(any(.dep_kinds[]; .kind == null))
                    | .pkg) as $dep (.;
              if .seen[$dep] then . else .seen[$dep] = true | .queue += [$dep] end))
      | .seen
      | keys[]
      | $packages[.]
      | select(.manifest_path | startswith($zed))
      | .name' |
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
