#!/usr/bin/env bash
# Sets up the GitHub fixtures in rozed-dev/e2e that rozed's revisions and stacks features are
# checked against, as rozed-bot. README.md beside this script has the fixture map.
#
# Usage: script/e2e/fixtures.sh <setup | status [--json] | push-r4 | rebase-r5 | reset-e1>
#
# Needs rozed-bot's token in ROZED_BOT_TOKEN. gh runs from ROZED_E2E_GH (default
# /opt/homebrew/bin/gh) with its own empty config, so it never picks up another account. Git runs
# in a scratch clone in a temporary folder, without your git config, and pushes over HTTPS with a
# credential helper that reads the token from the environment, so the token never reaches a remote
# URL, a config file or a command line.
#
# setup, push-r4 and rebase-r5 are idempotent: they find what exists by branch, PR and commit
# subject, and only add what's missing. reset-e1 makes a new E1 each time. The script never pushes
# to main, force-pushes or deletes anything; main moves only through merged PRs, and r5's rebase is
# GitHub's own. Writes go only to rozed-dev/e2e.
set -euo pipefail

readonly OWNER=rozed-dev NAME=e2e
readonly REPO=$OWNER/$NAME
readonly BOT=rozed-bot
readonly BOT_EMAIL=353226052+rozed-bot@users.noreply.github.com
GH=${ROZED_E2E_GH:-/opt/homebrew/bin/gh}

# E1's commit subjects, by revision. Revisions are found on a branch by these subjects.
e1_subject() {
  case $1 in
    1) echo "test(e1): r1 adds a.txt and b.txt" ;;
    2) echo "test(e1): r2 changes a.txt" ;;
    3) echo "test(e1): r3 changes b.txt" ;;
    4) echo "test(e1): r4 changes a.txt and b.txt" ;;
  esac
}
readonly E1_REVIEW="rozed-bot's review of r2. Fixture E1 checks that rozed marks this revision as your last review."
readonly E2_LAYERS="a b c e d s1 s2"

usage() {
  cat <<'EOF'
Usage: script/e2e/fixtures.sh <command>

  setup           create any fixture (E1, E2, E3) that's missing
  status [--json] print each fixture's PRs, branches and heads
  push-r4         push E1's next normal revision, r4
  rebase-r5       move main one commit, then rebase E1's PR on GitHub (r5)
  reset-e1        close E1's PR and open a fresh one on a new branch

Needs rozed-bot's token in ROZED_BOT_TOKEN, for example:
  (set -a; . ~/.config/rozed-dev/bot.env; set +a; script/e2e/fixtures.sh setup)
EOF
}

say() { echo "$*" >&2; }
die() {
  say "fixtures.sh: $*"
  exit 1
}

# Runs a command until it succeeds, up to <tries> times, 3 seconds apart.
retry() {
  local tries=$1 i=1
  shift
  until "$@"; do
    ((i < tries)) || return 1
    i=$((i + 1))
    sleep 3
  done
}

gh_api() { "$GH" api -H 'X-GitHub-Api-Version: 2022-11-28' "$@"; }

# Environment and identity checks every command needs. Sets `tmp`, removed on exit.
prepare() {
  [[ -n ${ROZED_BOT_TOKEN:-} ]] || die "ROZED_BOT_TOKEN isn't set (see --help)"
  [[ -x $GH ]] || die "no gh at $GH (set ROZED_E2E_GH)"
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/rozed-e2e.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  export ROZED_BOT_TOKEN GH_TOKEN=$ROZED_BOT_TOKEN GH_CONFIG_DIR=$tmp/gh
  export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1
  unset GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN GH_HOST
  local login
  login=$(gh_api user --jq .login)
  [[ $login == "$BOT" ]] || die "the token belongs to $login, not $BOT"
}

# --- Git, in a scratch clone ------------------------------------------------------------------

work=
CHANGED=0

clone() {
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
  export GIT_AUTHOR_NAME=$BOT GIT_AUTHOR_EMAIL=$BOT_EMAIL
  export GIT_COMMITTER_NAME=$BOT GIT_COMMITTER_EMAIL=$BOT_EMAIL
  work=$tmp/e2e
  git clone --quiet "https://github.com/$REPO.git" "$work"
}

g() { git -C "$work" "$@"; }

# Pushes HEAD to <branch>, a normal push that creates the branch or fast-forwards it. GitHub's
# "Create a pull request" hint is shown only when the push fails.
push() {
  local out
  # The helper's own shell expands $ROZED_BOT_TOKEN, so the token never appears in argv.
  # shellcheck disable=SC2016
  if ! out=$(g -c credential.helper= \
    -c 'credential.helper=!f() { [ "$1" = get ] || return 0; echo username=x-access-token; echo "password=$ROZED_BOT_TOKEN"; }; f' \
    push --quiet origin "HEAD:refs/heads/$1" 2>&1); then
    say "$out"
    die "couldn't push $1"
  fi
  g update-ref "refs/remotes/origin/$1" HEAD
  CHANGED=$((CHANGED + 1))
}

has_branch() { g rev-parse --verify --quiet "refs/remotes/origin/$1" >/dev/null; }
tip() { g rev-parse "refs/remotes/origin/$1"; }
start_at() { g checkout --quiet --detach "refs/remotes/origin/$1"; }

commit() {
  g add --all
  g commit --quiet --message "$1"
}

# Replaces line <n> of <file> in the scratch clone.
set_line() {
  awk -v n="$2" -v text="$3" 'NR == n { print text; next } { print }' "$work/$1" >"$work/$1.new"
  mv "$work/$1.new" "$work/$1"
}

# --- GitHub ------------------------------------------------------------------------------------

# The newest PR from <branch>, in any state: its number, or nothing.
pr_number() { gh_api "repos/$REPO/pulls?state=all&head=$OWNER:$1" --jq '.[0].number // empty'; }

# A PR's state: open, closed or merged.
pr_state() {
  gh_api "repos/$REPO/pulls/$1" --jq 'if .merged_at then "merged" else .state end'
}

pr_head() { gh_api "repos/$REPO/pulls/$1" --jq .head.sha; }

# Opens a PR and sets PR to its number.
open_pr() {
  PR=$(gh_api -X POST "repos/$REPO/pulls" -f head="$1" -f base="$2" -f title="$3" -f body="$4" \
    --jq .number)
  CHANGED=$((CHANGED + 1))
  say "opened #$PR: $1 -> $2"
  # rozed orders a layer's children by createdAt, which has one-second resolution.
  sleep 1
}

mergeable() { [[ $(gh_api "repos/$REPO/pulls/$1" --jq .mergeable) == true ]]; }

squash_merge() {
  gh_api -X PUT "repos/$REPO/pulls/$1/merge" -f merge_method=squash -f sha="$2" --silent
}

# Squash-merges PR <n> at its current head, unless it's merged. Its branch stays.
merge_pr() {
  local head
  [[ $(pr_state "$1") == merged ]] && return 0
  head=$(pr_head "$1")
  # GitHub works out whether a PR can merge in the background, after it's opened.
  retry 20 mergeable "$1" || die "#$1 never became mergeable"
  retry 5 squash_merge "$1" "$head" || die "couldn't merge #$1"
  CHANGED=$((CHANGED + 1))
  say "squash-merged #$1"
}

head_is() { [[ $(pr_head "$1") == "$2" ]]; }

# Waits until GitHub has moved PR <n>'s head to <sha>.
wait_for_head() { retry 20 head_is "$1" "$2" || die "#$1's head never reached $2"; }

# --- E1: revisions -----------------------------------------------------------------------------

# E1's branches are e1/revisions, then e1/revisions-2, -3 and so on after each reset.
e1_branch_name() { if (($1 == 1)); then echo e1/revisions; else echo "e1/revisions-$1"; fi; }

# The highest E1 branch number among the branch names on stdin, or 0.
e1_max() {
  awk '/^e1\/revisions(-[0-9]+)?$/ {
    n = 1
    if (match($0, /-[0-9]+$/)) n = substr($0, RSTART + 1) + 0
    if (n > max) max = n
  } END { print max + 0 }'
}

e1_local_max() { g for-each-ref --format='%(refname:strip=3)' refs/remotes/origin/e1/ | e1_max; }

# The SHA of E1 revision <n>'s commit on <branch>, or nothing.
e1_commit() {
  local subject
  has_branch "$2" || return 0
  subject=$(e1_subject "$1")
  g log --format='%H %s' "refs/remotes/origin/main..refs/remotes/origin/$2" |
    awk -v s="$subject" 'substr($0, 42) == s { print $1 }'
}

e1_has() { [[ -n $(e1_commit "$1" "$2") ]]; }

e1_reviewed() {
  local count
  count=$(gh_api "repos/$REPO/pulls/$1/reviews?per_page=100" |
    jq --arg bot "$BOT" --arg body "$E1_REVIEW" \
      '[.[] | select(.user.login == $bot and .body == $body)] | length')
  ((count > 0))
}

# Makes revision <n>'s change to e1/a.txt and e1/b.txt. r1 adds them, so later revisions change
# lines.
e1_change() {
  case $1 in
    1)
      local i
      mkdir -p "$work/e1"
      for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo "a: line $i"; done >"$work/e1/a.txt"
      for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo "b: line $i"; done >"$work/e1/b.txt"
      ;;
    2) set_line e1/a.txt 6 "a: line 6, changed in r2" ;;
    3) set_line e1/b.txt 6 "b: line 6, changed in r3" ;;
    4)
      set_line e1/a.txt 10 "a: line 10, changed in r4"
      set_line e1/b.txt 10 "b: line 10, changed in r4"
      ;;
  esac
}

# Pushes E1 revision <n> onto <branch> unless it's there, then waits for PR <pr> to see it.
e1_push() {
  local n=$1 branch=$2 pr=${3:-}
  e1_has "$n" "$branch" && return 0
  if has_branch "$branch"; then start_at "$branch"; else start_at main; fi
  e1_change "$n"
  commit "$(e1_subject "$n")"
  push "$branch"
  say "pushed r$n to $branch"
  [[ -z $pr ]] || wait_for_head "$pr" "$(tip "$branch")"
}

# Builds E1 on its newest branch, or on a new one when that branch's PR was closed.
e1_setup() {
  local max branch pr
  max=$(e1_local_max)
  ((max > 0)) || max=1
  branch=$(e1_branch_name "$max")
  pr=$(pr_number "$branch")
  if [[ -n $pr && $(pr_state "$pr") != open ]]; then
    branch=$(e1_branch_name $((max + 1)))
    pr=
  fi
  e1_push 1 "$branch"
  if [[ -z $pr ]]; then
    open_pr "$branch" main "E1: revisions" "Fixture E1 (revisions). Three normal pushes, r1 to r3, each changing \`e1/a.txt\` or \`e1/b.txt\`, and rozed-bot's COMMENT review on r2. \`push-r4\` adds r4, touching both files, and \`rebase-r5\` moves \`main\` and rebases this PR on GitHub. Made by \`script/e2e/fixtures.sh\` in scornz/rozed."
    pr=$PR
  fi
  e1_push 2 "$branch" "$pr"
  if ! e1_reviewed "$pr"; then
    gh_api -X POST "repos/$REPO/pulls/$pr/reviews" -f event=COMMENT \
      -f commit_id="$(e1_commit 2 "$branch")" -f body="$E1_REVIEW" --silent
    CHANGED=$((CHANGED + 1))
    say "reviewed r2 on #$pr"
  fi
  e1_push 3 "$branch" "$pr"
}

# The open E1 PR on the newest branch, with r3 pushed. Sets E1_BRANCH and E1_PR.
e1_current() {
  local max
  max=$(e1_local_max)
  ((max > 0)) || die "E1 doesn't exist yet; run setup"
  E1_BRANCH=$(e1_branch_name "$max")
  E1_PR=$(pr_number "$E1_BRANCH")
  [[ -n $E1_PR && $(pr_state "$E1_PR") == open ]] || die "$E1_BRANCH has no open PR; run setup"
  e1_has 3 "$E1_BRANCH" || die "$E1_BRANCH has no r3 yet; run setup"
}

# The force-pushes in PR <n>'s timeline, as a JSON array.
force_pushes() {
  # shellcheck disable=SC2016 # GraphQL variables, not shell ones
  gh_api graphql -f owner="$OWNER" -f name="$NAME" -F number="$1" -f query='
    query($owner: String!, $name: String!, $number: Int!) {
      repository(owner: $owner, name: $name) {
        pullRequest(number: $number) {
          timelineItems(first: 100, itemTypes: [HEAD_REF_FORCE_PUSHED_EVENT]) {
            nodes {
              ... on HeadRefForcePushedEvent {
                createdAt actor { login } beforeCommit { oid } afterCommit { oid }
              }
            }
          }
        }
      }
    }' --jq '[.data.repository.pullRequest.timelineItems.nodes[]
      | {at: .createdAt, actor: .actor.login, before: .beforeCommit.oid, after: .afterCommit.oid}]'
}

cmd_push_r4() {
  e1_current
  if e1_has 4 "$E1_BRANCH"; then
    say "E1 #$E1_PR already has r4."
    return
  fi
  e1_push 4 "$E1_BRANCH" "$E1_PR"
}

rebase_e1() {
  # shellcheck disable=SC2016 # GraphQL variables, not shell ones
  gh_api graphql -f id="$1" -f head="$2" -f query='
    mutation($id: ID!, $head: GitObjectID!) {
      updatePullRequestBranch(
        input: {pullRequestId: $id, updateMethod: REBASE, expectedHeadOid: $head}
      ) { pullRequest { headRefOid } }
    }' --silent
}

head_moved() { [[ $(pr_head "$1") != "$2" ]]; }

cmd_rebase_r5() {
  e1_current
  e1_has 4 "$E1_BRANCH" || die "E1 #$E1_PR has no r4 yet; run push-r4 first"
  if [[ $(force_pushes "$E1_PR" | jq length) -gt 0 ]]; then
    say "E1 #$E1_PR was already rebased (r5)."
    return
  fi
  # main moves through a one-commit PR, squash-merged, touching only e1/base.txt, which no E1
  # revision changes, so the rebase can't conflict. e1/revisions-2 moves main with e1/move-main-2.
  local move=${E1_BRANCH/revisions/move-main} pr head id
  if ! has_branch "$move"; then
    start_at main
    mkdir -p "$work/e1"
    echo "main moved under $E1_BRANCH, so its r5 is a rebase." >>"$work/e1/base.txt"
    commit "test(e1): move main under $E1_BRANCH"
    push "$move"
    say "pushed $move"
  fi
  pr=$(pr_number "$move")
  if [[ -z $pr ]]; then
    open_pr "$move" main "E1: move main under $E1_BRANCH" "Moves \`main\` one commit, so fixture E1's server-side rebase (r5) has a base that moved. Touches only \`e1/base.txt\`, which E1 never changes. Made by \`script/e2e/fixtures.sh\` in scornz/rozed."
    pr=$PR
  fi
  merge_pr "$pr"
  head=$(pr_head "$E1_PR")
  id=$(gh_api "repos/$REPO/pulls/$E1_PR" --jq .node_id)
  # GitHub may not see main's new commit for a moment, and refuses to rebase a PR that isn't behind.
  retry 10 rebase_e1 "$id" "$head" || die "GitHub didn't rebase #$E1_PR"
  CHANGED=$((CHANGED + 1))
  retry 20 head_moved "$E1_PR" "$head" || die "#$E1_PR's head didn't move after the rebase"
  say "rebased #$E1_PR on GitHub: $head -> $(pr_head "$E1_PR")"
}

cmd_reset_e1() {
  local max branch pr
  max=$(e1_local_max)
  if ((max > 0)); then
    branch=$(e1_branch_name "$max")
    pr=$(pr_number "$branch")
    if [[ -n $pr && $(pr_state "$pr") == open ]]; then
      gh_api -X PATCH "repos/$REPO/pulls/$pr" -f state=closed --silent
      CHANGED=$((CHANGED + 1))
      say "closed #$pr ($branch)"
    fi
  fi
  e1_setup
}

# --- E2: stack tree ----------------------------------------------------------------------------

# The branch a layer is built on: d sits on c's head but targets main.
e2_parent() {
  case $1 in
    a | s1) echo main ;;
    b) echo e2/a ;;
    c | e) echo e2/b ;;
    d) echo e2/c ;;
    s2) echo e2/s1 ;;
  esac
}

e2_base() {
  case $1 in
    d) echo main ;;
    *) e2_parent "$1" ;;
  esac
}

e2_body() {
  local what
  case $1 in
    a) what="the bottom of the base-branch chain a, b, c" ;;
    b) what="the middle of the chain, with two children: c, then e" ;;
    c) what="the top of the chain" ;;
    e) what="a sibling of c, opened after it" ;;
    d) what="built on e2/c's head but targeting main, so only the shared-commits rule links it to c" ;;
    s1) what="the bottom of a native GitHub stack" ;;
    s2) what="the top of a native GitHub stack" ;;
  esac
  echo "Fixture E2 (stack tree), layer $1: $what. Made by \`script/e2e/fixtures.sh\` in scornz/rozed."
}

e2_setup() {
  local layer branch pr s1='' s2='' stack body
  for layer in $E2_LAYERS; do
    branch=e2/$layer
    if ! has_branch "$branch"; then
      start_at "$(e2_parent "$layer")"
      mkdir -p "$work/e2"
      printf 'Layer %s of fixture E2.\n' "$layer" >"$work/e2/$layer.txt"
      commit "test(e2): add layer $layer"
      push "$branch"
      say "pushed $branch"
    fi
    pr=$(pr_number "$branch")
    if [[ -z $pr ]]; then
      open_pr "$branch" "$(e2_base "$layer")" "E2: layer $layer" "$(e2_body "$layer")"
      pr=$PR
    fi
    case $layer in
      s1) s1=$pr ;;
      s2) s2=$pr ;;
    esac
  done
  stack=$(gh_api "repos/$REPO/stacks?pull_request=$s1" --jq '.[0].number // empty')
  if [[ -z $stack ]]; then
    body=$(jq -n --argjson s1 "$s1" --argjson s2 "$s2" '{pull_requests: [$s1, $s2]}')
    stack=$(gh_api -X POST "repos/$REPO/stacks" --input - --jq .number <<<"$body")
    CHANGED=$((CHANGED + 1))
    say "made stack #$stack from #$s1 and #$s2"
  fi
}

# --- E3: needs rebase --------------------------------------------------------------------------

e3_setup() {
  local x y
  if ! has_branch e3/x; then
    start_at main
    mkdir -p "$work/e3"
    printf 'x: line 1\n' >"$work/e3/x.txt"
    commit "test(e3): add x.txt"
    printf 'x: line 2\n' >>"$work/e3/x.txt"
    commit "test(e3): extend x.txt"
    push e3/x
    say "pushed e3/x"
  fi
  if ! has_branch e3/y; then
    start_at e3/x
    printf 'y: line 1\n' >"$work/e3/y.txt"
    commit "test(e3): add y.txt on top of x"
    push e3/y
    say "pushed e3/y"
  fi
  x=$(pr_number e3/x)
  if [[ -z $x ]]; then
    open_pr e3/x main "E3: x" "Fixture E3 (needs rebase), the lower layer: two commits, squash-merged into main. Its branch is kept. Made by \`script/e2e/fixtures.sh\` in scornz/rozed."
    x=$PR
  fi
  y=$(pr_number e3/y)
  if [[ -z $y ]]; then
    open_pr e3/y e3/x "E3: y" "Fixture E3 (needs rebase), the upper layer: one commit on top of e3/x, which was squash-merged, so this layer needs a rebase. Made by \`script/e2e/fixtures.sh\` in scornz/rozed."
    y=$PR
  fi
  if [[ $(pr_state "$x") != merged ]]; then
    merge_pr "$x"
    say "#$y now targets $(gh_api "repos/$REPO/pulls/$y" --jq .base.ref)"
  fi
}

cmd_setup() {
  clone
  e1_setup
  e2_setup
  e3_setup
  if ((CHANGED == 0)); then say "Nothing to change: every fixture exists."; fi
}

# --- status ------------------------------------------------------------------------------------

status_json() {
  local prs branches max e1_branch e1_pr e1_head pushes review revisions stack s1
  prs=$(gh_api --paginate "repos/$REPO/pulls?state=all&per_page=100" | jq -s 'add // [] | map({
    pr: .number, branch: .head.ref, base: .base.ref, head: .head.sha,
    state: (if .merged_at then "merged" else .state end),
    merge_commit: (if .merged_at then .merge_commit_sha else null end)})')
  branches=$(gh_api --paginate "repos/$REPO/branches?per_page=100" --jq '.[].name')

  local e1=null
  max=$(e1_max <<<"$branches")
  if ((max > 0)); then
    e1_branch=$(e1_branch_name "$max")
    e1_pr=$(jq --arg b "$e1_branch" 'first(.[] | select(.branch == $b) | .pr) // empty' <<<"$prs")
    pushes='[]' review=null revisions='{}'
    if [[ -n $e1_pr ]]; then
      pushes=$(force_pushes "$e1_pr")
      review=$(gh_api "repos/$REPO/pulls/$e1_pr/reviews?per_page=100" |
        jq --arg bot "$BOT" --arg body "$E1_REVIEW" \
          'first(.[] | select(.user.login == $bot and .body == $body) | {id, commit: .commit_id}) // null')
      # r1 to r4 as pushed: the PR's commits, or after a rebase, the commits up to the head it
      # replaced. r5 is the rebase's result.
      e1_head=$(jq -r 'if length > 0 then .[0].before else empty end' <<<"$pushes")
      [[ -n $e1_head ]] || e1_head=$(pr_head "$e1_pr")
      revisions=$(gh_api "repos/$REPO/compare/main...$e1_head" | jq --argjson pushes "$pushes" '
        [.commits[] | (.commit.message | split("\n")[0]
          | capture("^test\\(e1\\): (?<r>r[0-9]+) ").r) as $r | {($r): .sha}] | add // {}
        | if ($pushes | length) > 0 then . + {r5: $pushes[0].after} else . end')
    fi
    e1=$(jq --arg b "$e1_branch" --arg move "${e1_branch/revisions/move-main}" \
      --argjson revisions "$revisions" --argjson review "$review" --argjson pushes "$pushes" '
      (first(.[] | select(.branch == $b)) // {pr: null, branch: $b})
      + {revisions: $revisions, review: $review, force_pushes: $pushes,
         main_move: (first(.[] | select(.branch == $move) | {pr, branch, state, merge_commit}) // null),
         earlier: [.[] | select(.branch != $b and (.branch | test("^e1/revisions(-[0-9]+)?$")))
                   | {pr, branch, state}]}
      | del(.merge_commit)' <<<"$prs")
  fi

  s1=$(jq 'first(.[] | select(.branch == "e2/s1") | .pr) // empty' <<<"$prs")
  stack=null
  if [[ -n $s1 ]]; then
    stack=$(gh_api "repos/$REPO/stacks?pull_request=$s1" |
      jq 'first(.[] | {number, prs: [.pull_requests[].number]}) // null')
  fi

  jq --arg main "$(gh_api "repos/$REPO/branches/main" --jq .commit.sha)" --argjson e1 "$e1" \
    --argjson stack "$stack" --arg layers "$E2_LAYERS" '
    . as $prs
    | def layer($b): first($prs[] | select(.branch == $b)) // null;
    {
      repo: "rozed-dev/e2e",
      main: $main,
      e1: $e1,
      e2: (reduce ($layers | split(" ")[]) as $l ({}; .[$l] = (layer("e2/" + $l) | if . then del(.merge_commit) else . end))
           + {stack: $stack}),
      e3: {
        x: layer("e3/x"),
        y: (layer("e3/y") | if . then del(.merge_commit) + {retargeted: (.base != "e3/x")} else . end)
      }
    }' <<<"$prs"
}

status_text() {
  jq -r '
    def short: if . then .[0:7] else "-" end;
    def row($name): if . == null then "  \($name)  missing"
      else "  \($name)  #\(.pr)  \(.branch) -> \(.base)  \(.state)  head \(.head | short)" end;
    "main  \(.main | short)",
    "E1 revisions",
    (if .e1 == null then "  missing" else
      (.e1 | row("e1")),
      "      " + ([.e1.revisions | to_entries[] | "\(.key) \(.value | short)"] | join("  ")),
      (if .e1.review then "      review \(.e1.review.id) on \(.e1.review.commit | short)" else "      no review" end),
      (if .e1.main_move then "      main moved by #\(.e1.main_move.pr) (\(.e1.main_move.state)) to \(.e1.main_move.merge_commit | short)" else empty end),
      (.e1.force_pushes[] | "      force-push \(.before | short) -> \(.after | short) by \(.actor) at \(.at)"),
      (.e1.earlier[] | "      earlier #\(.pr) \(.branch) \(.state)")
    end),
    "E2 stack tree",
    (.e2 | to_entries[] | select(.key != "stack") | .key as $k | .value | row($k)),
    (if .e2.stack then "  native stack #\(.e2.stack.number): \(.e2.stack.prs | map("#\(.)") | join(" "))"
     else "  no native stack" end),
    "E3 needs rebase",
    (.e3.x | row("x")),
    (if .e3.x.merge_commit then "      merge commit \(.e3.x.merge_commit | short)" else empty end),
    (.e3.y | row("y")),
    (if .e3.y then "      retargeted: \(.e3.y.retargeted)" else empty end)'
}

cmd_status() {
  case ${1:-} in
    --json) status_json ;;
    '') status_json | status_text ;;
    *) die "status takes only --json" ;;
  esac
}

main() {
  local cmd=${1:-}
  if [[ $# -gt 0 ]]; then shift; fi
  case $cmd in
    -h | --help | help)
      usage
      return
      ;;
    setup | push-r4 | rebase-r5 | reset-e1) [[ $# -eq 0 ]] || die "$cmd takes no arguments" ;;
    status) ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  prepare
  case $cmd in
    setup) cmd_setup ;;
    status) cmd_status "$@" ;;
    push-r4) clone && cmd_push_r4 ;;
    rebase-r5) clone && cmd_rebase_r5 ;;
    reset-e1) clone && cmd_reset_e1 ;;
  esac
}

main "$@"
