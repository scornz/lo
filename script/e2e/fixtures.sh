#!/usr/bin/env bash
# Sets up the GitHub fixtures in rozed-dev/e2e that Lo is checked against, as rozed-bot: revisions
# and stacks (E1–E3), code navigation and trust (E6, E8), moved code and the syntax-aware view
# (E9), performance (E10), languages from bundled extensions (E12) and image previews (#17).
# trees.sh beside this script writes the files of E6 onwards, and README.md has the fixture map.
#
# Usage: script/e2e/fixtures.sh <setup | status [--json] | push-r4 | rebase-r5 | push-r6 | reset-e1>
#
# Needs rozed-bot's token in ROZED_BOT_TOKEN. gh runs from LO_E2E_GH (default
# /opt/homebrew/bin/gh) with its own empty config, so it never picks up another account. Git runs
# in a scratch clone in a temporary folder, with none of your git config or GIT_* variables, and
# pushes over HTTPS with a credential helper that reads the token from the environment, so the
# token never reaches a remote URL, a config file or a command line.
#
# setup, push-r4, rebase-r5 and push-r6 are idempotent: they find what exists by branch, PR,
# commit subject and review, and only add what's missing. reset-e1 makes a new E1 each time. The
# script never pushes to main, force-pushes or deletes anything; main moves only through merged
# PRs, and r5's rebase is GitHub's own. Writes go only to rozed-dev/e2e. A failed GitHub request
# stops it.
set -euo pipefail

# The token is in this script's environment, so a trace would print it.
case $- in
  *x*)
    echo "fixtures.sh: don't run this with -x: the trace would print the token" >&2
    exit 2
    ;;
  *) ;;
esac

readonly OWNER=rozed-dev NAME=e2e
readonly REPO=$OWNER/$NAME
readonly BOT=rozed-bot
readonly BOT_EMAIL=353226052+rozed-bot@users.noreply.github.com
GH=${LO_E2E_GH:-/opt/homebrew/bin/gh}

# setup and status find the bot's review by this text, so it keeps the project's first name.
readonly E1_REVIEW="rozed-bot's review of r2. Fixture E1 checks that rozed marks this revision as your last review."
readonly E2_LAYERS="a b c e d s1 s2"
# Maps a compare response's commits to {"r1": sha, …} by E1's commit subjects.
# shellcheck disable=SC2016 # jq variables, not shell ones
readonly E1_REVISIONS='[.commits[] | (.commit.message | split("\n")[0]
  | capture("^test\\(e1\\): (?<r>r[0-9]+) ").r) as $r | {($r): .sha}] | add // {}'

# E9's review, found by its text, and its two threads, placed by the text of their lines.
readonly E9_REVIEW="Fixture E9's review: a thread on a moved line, and one on a line only the old side has. Made by script/e2e/fixtures.sh in scornz/lo."
readonly E9_MOVED_LINE='    const middle = user.middle?.trim();'
readonly E9_MOVED_COMMENT="Fixture E9: a thread on a moved line. Lo shows it under this line in the diff and in the syntax-aware view."
# shellcheck disable=SC2016 # TypeScript, not shell
readonly E9_OLD_LINE='  return `${user.last.toUpperCase()}, ${user.first}`;'
readonly E9_OLD_COMMENT="Fixture E9: a thread on a line only the old side has."

# shellcheck source-path=SCRIPTDIR source=trees.sh
. "$(dirname "${BASH_SOURCE[0]}")/trees.sh"

usage() {
  cat <<'EOF'
Usage: script/e2e/fixtures.sh <command>

  setup           create any fixture that's missing: E1–E3, E6, E8–E10, E12 and #17
  status [--json] print each fixture's PRs, branches and heads
  push-r4         push E1's r4, a normal push
  rebase-r5       move main one commit, then rebase E1's PR on GitHub (r5)
  push-r6         push E1's r6, a normal push after the rebase
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

# gh gets the token for each call, and nothing else inherits it as GH_TOKEN.
gh_api() { GH_TOKEN=$ROZED_BOT_TOKEN "$GH" api -H 'X-GitHub-Api-Version: 2022-11-28' "$@"; }

# Environment and identity checks every command needs. Sets `tmp`, removed on exit.
prepare() {
  local tool login
  [[ -n ${ROZED_BOT_TOKEN:+set} ]] || die "ROZED_BOT_TOKEN isn't set (see --help)"
  [[ -x $GH ]] || die "no gh at $GH (set LO_E2E_GH)"
  for tool in git jq awk base64; do
    command -v "$tool" >/dev/null || die "needs $tool"
  done
  command -v sha256sum >/dev/null || command -v shasum >/dev/null ||
    die "needs sha256sum or shasum"
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/lo-e2e.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  # The credential helper reads ROZED_BOT_TOKEN, so git's children need it.
  export ROZED_BOT_TOKEN GH_CONFIG_DIR=$tmp/gh GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1
  unset GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN GH_HOST
  login=$(gh_api user --jq .login)
  [[ $login == "$BOT" ]] || die "the token belongs to $login, not $BOT"
}

# --- Git, in a scratch clone ------------------------------------------------------------------

work=
CHANGED=0

clone() {
  local var
  # Nothing from the caller steers git: no repository, config, askpass, ssh command or tracing.
  for var in $(compgen -v GIT_ || true); do unset "$var"; done
  unset SSH_ASKPASS
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_ATTR_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
  # A home of its own, so git reads none of yours: not the ignore and attributes files under
  # ~/.config/git, which apply whatever the config says, and not ~/.netrc, which curl would answer
  # GitHub's 401 with before git asks the credential helper. gh keeps its own config folder.
  mkdir -p "$tmp/home"
  export HOME=$tmp/home
  unset XDG_CONFIG_HOME
  export GIT_AUTHOR_NAME=$BOT GIT_AUTHOR_EMAIL=$BOT_EMAIL
  export GIT_COMMITTER_NAME=$BOT GIT_COMMITTER_EMAIL=$BOT_EMAIL
  work=$tmp/e2e
  git -c core.askPass= clone --quiet "https://github.com/$REPO.git" "$work"
}

g() { git -C "$work" -c core.askPass= "$@"; }

# Pushes HEAD to <branch>, a normal push that creates the branch or fast-forwards it. GitHub's
# "Create a pull request" hint is shown only when the push fails.
push() {
  local out
  # The helper's own shell expands $ROZED_BOT_TOKEN, so the token never appears in argv, and
  # `set +x` keeps an inherited SHELLOPTS=xtrace from tracing it.
  # shellcheck disable=SC2016
  if ! out=$(g -c credential.helper= \
    -c 'credential.helper=!f() { set +x; [ "$1" = get ] || return 0; echo username=x-access-token; echo "password=$ROZED_BOT_TOKEN"; }; f' \
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

# Commits every file in the scratch clone, and stops if anything was left out: an ignored file
# would mean a fixture pushed without it.
commit() {
  local left
  g add --all
  g commit --quiet --message "$1"
  left=$(g status --porcelain --ignored)
  [[ -z $left ]] || die "these files weren't committed: $left"
}

# Replaces line <n> of <file> in the scratch clone.
set_line() {
  TEXT=$3 awk -v n="$2" 'NR == n { print ENVIRON["TEXT"]; next } { print }' "$work/$1" >"$work/$1.new"
  mv "$work/$1.new" "$work/$1"
}

# --- GitHub ------------------------------------------------------------------------------------
#
# Every $(…) is assigned to a variable before it's tested, so a failed request stops the script
# instead of reading as "missing" and making a duplicate. Bash doesn't apply `set -e` inside a
# function run in $(…), so such functions return on each failed step themselves.

# The newest PR rozed-bot opened from <branch> in rozed-dev/e2e, in any state: its number, or
# nothing. PRs from forks or other authors never count.
pr_number() {
  gh_api "repos/$REPO/pulls?state=all&head=$OWNER:$1" --jq "first(.[]
    | select(.user.login == \"$BOT\" and .head.repo.full_name == \"$REPO\") | .number) // empty"
}

# A PR's state: open, closed or merged.
pr_state() {
  gh_api "repos/$REPO/pulls/$1" --jq 'if .merged_at then "merged" else .state end'
}

pr_head() { gh_api "repos/$REPO/pulls/$1" --jq .head.sha; }

# How many of PR <n>'s reviews are rozed-bot's with exactly <body>.
review_count() {
  gh_api "repos/$REPO/pulls/$1/reviews?per_page=100" |
    jq --arg bot "$BOT" --arg body "$2" \
      '[.[] | select(.user.login == $bot and .body == $body)] | length'
}

# Opens a PR and sets PR to its number. A branch pushed a moment ago can be invisible to the API
# for a moment, and a failed POST may still have opened the PR, so it looks before trying again.
open_pr() {
  local attempt
  for attempt in 1 2 3; do
    if PR=$(gh_api -X POST "repos/$REPO/pulls" -f head="$1" -f base="$2" -f title="$3" \
      -f body="$4" --jq .number); then
      break
    fi
    ((attempt < 3)) || die "couldn't open a PR from $1"
    sleep 3
    PR=$(pr_number "$1")
    [[ -z $PR ]] || break
  done
  CHANGED=$((CHANGED + 1))
  say "opened #$PR: $1 -> $2"
  # Lo orders a layer's children by createdAt, which has one-second resolution.
  sleep 1
}

# Polls: a failed request reads as "not yet", and the caller dies when retry gives up.
mergeable() { [[ $(gh_api "repos/$REPO/pulls/$1" --jq .mergeable) == true ]]; }
head_is() { [[ $(pr_head "$1") == "$2" ]]; }
head_moved() { [[ $(pr_head "$1") != "$2" ]]; }
merged() { [[ $(pr_state "$1") == merged ]]; }

squash_merge() {
  gh_api -X PUT "repos/$REPO/pulls/$1/merge" -f merge_method=squash -f sha="$2" --silent || merged "$1"
}

# Squash-merges PR <n> at its current head, unless it's merged. It must be rozed-bot's PR from
# rozed-dev/e2e into <base>, titled <title>. Its branch stays.
merge_pr() {
  local pr=$1 info state head
  info=$(gh_api "repos/$REPO/pulls/$pr")
  jq -e --arg bot "$BOT" --arg repo "$REPO" --arg base "$2" --arg title "$3" '
    .user.login == $bot and .head.repo.full_name == $repo and .base.ref == $base
    and .title == $title' <<<"$info" >/dev/null ||
    die "#$pr isn't the PR the script opened: its author, repository, base or title differ"
  state=$(jq -r 'if .merged_at then "merged" else .state end' <<<"$info")
  [[ $state != merged ]] || return 0
  [[ $state == open ]] || die "#$pr is closed"
  head=$(jq -r .head.sha <<<"$info")
  # GitHub works out whether a PR can merge in the background, after it's opened.
  retry 20 mergeable "$pr" || die "#$pr never became mergeable"
  retry 5 squash_merge "$pr" "$head" || die "couldn't merge #$pr"
  CHANGED=$((CHANGED + 1))
  say "squash-merged #$pr"
}

# Waits until GitHub has moved PR <n>'s head to <sha>.
wait_for_head() { retry 20 head_is "$1" "$2" || die "#$1's head never reached $2"; }

# --- E1: revisions -----------------------------------------------------------------------------

# E1's commit subjects, by revision. Revisions are found on a branch by these subjects. r5 is
# GitHub's rebase, so it has no commit of its own.
e1_subject() {
  case $1 in
    1) echo "test(e1): r1 adds a.txt and b.txt" ;;
    2) echo "test(e1): r2 changes a.txt" ;;
    3) echo "test(e1): r3 changes b.txt" ;;
    4) echo "test(e1): r4 changes a.txt and b.txt" ;;
    6) echo "test(e1): r6 changes a.txt" ;;
    *) die "E1 has no pushed revision r$1" ;;
  esac
}

# Makes revision <n>'s change to e1/a.txt and e1/b.txt. r1 adds them, so later revisions change
# lines.
e1_change() {
  local i
  case $1 in
    1)
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
    6) set_line e1/a.txt 3 "a: line 3, changed in r6" ;;
    *) die "E1 has no pushed revision r$1" ;;
  esac
}

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
  subject=$(e1_subject "$1") || return 1
  g log --format='%H %s' "refs/remotes/origin/main..refs/remotes/origin/$2" |
    awk -v s="$subject" 'substr($0, 42) == s { print $1 }'
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

force_push_count() {
  local pushes
  pushes=$(force_pushes "$1") || return 1
  jq length <<<"$pushes"
}

# Pushes E1 revision <n> onto <branch> unless it's there, then waits for PR <pr> to see it.
e1_push() {
  local n=$1 branch=$2 pr=${3:-} sha subject head
  sha=$(e1_commit "$n" "$branch")
  [[ -z $sha ]] || return 0
  if has_branch "$branch"; then start_at "$branch"; else start_at main; fi
  e1_change "$n"
  subject=$(e1_subject "$n")
  commit "$subject"
  push "$branch"
  say "pushed r$n to $branch"
  if [[ -n $pr ]]; then
    head=$(tip "$branch")
    wait_for_head "$pr" "$head"
  fi
}

# Builds E1 through r3 on branch number <n>, or without <n> on its newest branch, or the next one
# when that branch's PR was closed.
e1_setup() {
  local max branch pr state count r2
  if [[ -n ${1:-} ]]; then
    branch=$(e1_branch_name "$1")
    pr=$(pr_number "$branch")
  else
    max=$(e1_local_max)
    ((max > 0)) || max=1
    branch=$(e1_branch_name "$max")
    pr=$(pr_number "$branch")
    if [[ -n $pr ]]; then
      state=$(pr_state "$pr")
      if [[ $state != open ]]; then
        branch=$(e1_branch_name $((max + 1)))
        pr=$(pr_number "$branch")
      fi
    fi
  fi
  if [[ -n $pr ]]; then
    state=$(pr_state "$pr")
    [[ $state == open ]] || die "#$pr ($branch) is $state"
  fi
  e1_push 1 "$branch"
  if [[ -z $pr ]]; then
    open_pr "$branch" main "E1: revisions" "Fixture E1 (revisions). Three normal pushes, r1 to r3, each changing \`e1/a.txt\` or \`e1/b.txt\`, and rozed-bot's COMMENT review on r2. \`push-r4\` adds r4, touching both files, \`rebase-r5\` moves \`main\` and rebases this PR on GitHub, and \`push-r6\` adds r6 after the rebase. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
    pr=$PR
  fi
  e1_push 2 "$branch" "$pr"
  count=$(review_count "$pr" "$E1_REVIEW")
  if ((count == 0)); then
    r2=$(e1_commit 2 "$branch")
    [[ -n $r2 ]] || die "can't find r2 on $branch"
    gh_api -X POST "repos/$REPO/pulls/$pr/reviews" -f event=COMMENT -f commit_id="$r2" \
      -f body="$E1_REVIEW" --silent
    CHANGED=$((CHANGED + 1))
    say "reviewed r2 on #$pr"
  fi
  e1_push 3 "$branch" "$pr"
}

# The open E1 PR on the newest branch, with r3 pushed. Sets E1_BRANCH and E1_PR.
e1_current() {
  local max state r3
  max=$(e1_local_max)
  ((max > 0)) || die "E1 doesn't exist yet; run setup"
  E1_BRANCH=$(e1_branch_name "$max")
  E1_PR=$(pr_number "$E1_BRANCH")
  [[ -n $E1_PR ]] || die "$E1_BRANCH has no PR; run setup"
  state=$(pr_state "$E1_PR")
  [[ $state == open ]] || die "#$E1_PR ($E1_BRANCH) is $state; run setup"
  r3=$(e1_commit 3 "$E1_BRANCH")
  [[ -n $r3 ]] || die "$E1_BRANCH has no r3 yet; run setup"
}

cmd_push_r4() {
  local r4
  e1_current
  r4=$(e1_commit 4 "$E1_BRANCH")
  if [[ -n $r4 ]]; then
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

# expectedHeadOid makes a repeat refuse once the first one has worked, so a lost response is
# checked against the head.
rebase_once() { rebase_e1 "$2" "$3" || head_moved "$1" "$3"; }

cmd_rebase_r5() {
  local r4 count move title pr head id after
  e1_current
  r4=$(e1_commit 4 "$E1_BRANCH")
  [[ -n $r4 ]] || die "E1 #$E1_PR has no r4 yet; run push-r4 first"
  count=$(force_push_count "$E1_PR")
  if ((count > 0)); then
    say "E1 #$E1_PR was already rebased (r5)."
    return
  fi
  # main moves through a one-commit PR, squash-merged, touching only e1/base.txt, which no E1
  # revision changes, so the rebase can't conflict. e1/revisions-2 moves main with e1/move-main-2.
  move=${E1_BRANCH/revisions/move-main}
  title="E1: move main under $E1_BRANCH"
  pr=$(pr_number "$move")
  if [[ -z $pr ]]; then
    if ! has_branch "$move"; then
      start_at main
      mkdir -p "$work/e1"
      echo "main moved under $E1_BRANCH, so its r5 is a rebase." >>"$work/e1/base.txt"
      commit "test(e1): move main under $E1_BRANCH"
      push "$move"
      say "pushed $move"
    fi
    open_pr "$move" main "$title" "Moves \`main\` one commit, so fixture E1's server-side rebase (r5) has a base that moved. Touches only \`e1/base.txt\`, which E1 never changes. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
    pr=$PR
  fi
  merge_pr "$pr" main "$title"
  head=$(pr_head "$E1_PR")
  id=$(gh_api "repos/$REPO/pulls/$E1_PR" --jq .node_id)
  # GitHub may not see main's new commit for a moment, and refuses to rebase a PR that isn't behind.
  retry 10 rebase_once "$E1_PR" "$id" "$head" || die "GitHub didn't rebase #$E1_PR"
  CHANGED=$((CHANGED + 1))
  retry 20 head_moved "$E1_PR" "$head" || die "#$E1_PR's head didn't move after the rebase"
  after=$(pr_head "$E1_PR")
  say "rebased #$E1_PR on GitHub: $head -> $after"
}

cmd_push_r6() {
  local count r6
  e1_current
  count=$(force_push_count "$E1_PR")
  ((count > 0)) || die "E1 #$E1_PR hasn't been rebased yet; run rebase-r5 first"
  r6=$(e1_commit 6 "$E1_BRANCH")
  if [[ -n $r6 ]]; then
    say "E1 #$E1_PR already has r6."
    return
  fi
  e1_push 6 "$E1_BRANCH" "$E1_PR"
}

# Closes the newest E1 PR if it's open, then builds the next number. If this fails partway, finish
# with setup: running reset-e1 again starts yet another E1.
cmd_reset_e1() {
  local max branch pr state
  max=$(e1_local_max)
  if ((max > 0)); then
    branch=$(e1_branch_name "$max")
    pr=$(pr_number "$branch")
    if [[ -n $pr ]]; then
      state=$(pr_state "$pr")
      if [[ $state == open ]]; then
        gh_api -X PATCH "repos/$REPO/pulls/$pr" -f state=closed --silent
        CHANGED=$((CHANGED + 1))
        say "closed #$pr ($branch)"
      fi
    fi
  fi
  e1_setup $((max + 1))
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
    *) die "E2 has no layer $1" ;;
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
    *) die "E2 has no layer $1" ;;
  esac
  echo "Fixture E2 (stack tree), layer $1: $what. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
}

e2_setup() {
  local layer branch parent base body pr s1='' s2='' stack request
  for layer in $E2_LAYERS; do
    branch=e2/$layer
    if ! has_branch "$branch"; then
      parent=$(e2_parent "$layer")
      start_at "$parent"
      mkdir -p "$work/e2"
      printf 'Layer %s of fixture E2.\n' "$layer" >"$work/e2/$layer.txt"
      commit "test(e2): add layer $layer"
      push "$branch"
      say "pushed $branch"
    fi
    pr=$(pr_number "$branch")
    if [[ -z $pr ]]; then
      base=$(e2_base "$layer")
      body=$(e2_body "$layer")
      open_pr "$branch" "$base" "E2: layer $layer" "$body"
      pr=$PR
    fi
    case $layer in
      s1) s1=$pr ;;
      s2) s2=$pr ;;
      *) ;;
    esac
  done
  stack=$(gh_api "repos/$REPO/stacks?pull_request=$s1" --jq '.[0].number // empty')
  if [[ -z $stack ]]; then
    request=$(jq -n --argjson s1 "$s1" --argjson s2 "$s2" '{pull_requests: [$s1, $s2]}')
    stack=$(gh_api -X POST "repos/$REPO/stacks" --input - --jq .number <<<"$request")
    CHANGED=$((CHANGED + 1))
    say "made stack #$stack from #$s1 and #$s2"
  fi
}

# --- E3: needs rebase --------------------------------------------------------------------------

e3_setup() {
  local x y state base
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
    open_pr e3/x main "E3: x" "Fixture E3 (needs rebase), the lower layer: two commits, squash-merged into main. Its branch is kept. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
    x=$PR
  fi
  y=$(pr_number e3/y)
  if [[ -z $y ]]; then
    open_pr e3/y e3/x "E3: y" "Fixture E3 (needs rebase), the upper layer: one commit on top of e3/x, which was squash-merged, so this layer needs a rebase. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
    y=$PR
  fi
  state=$(pr_state "$x")
  if [[ $state != merged ]]; then merge_pr "$x" main "E3: x"; fi
  # GitHub retargets y to main if e3/x is deleted, and E3 then stops needing a rebase.
  base=$(gh_api "repos/$REPO/pulls/$y" --jq .base.ref)
  [[ $base == e3/x ]] ||
    die "#$y targets $base, not e3/x; 'Automatically delete head branches' must stay off"
}

# --- E6–E12 and #17: code, hostile files, moves, large, languages and images -------------------
#
# trees.sh writes their files. E6, E9, E10 and E12 change code that has to exist first, so each has
# a base branch, e<n>/base, that adds its base side to main, and its PR targets that branch, not
# main. E8 has one too, holding only main, so merging it can't put its baits on main. main never
# moves for them, so E1's base still moves only under rebase-r5. #17 only adds harmless files, so it
# targets main. Like E2 and E3, they have no reset: setup accepts their PRs in any state, and
# builds only what's missing, except that it keeps E8 up to date (see e8_setup).

# Builds what's missing of a fixture whose PR changes files: <base> from main with the files' base
# side, <branch> from <base> with their head side, then the PR from <branch> into <base>. trees.sh's
# <fixture>_tree writes the files. Sets PR.
changed_tree_setup() {
  local fixture=$1 base=$2 branch=$3 subject=$4 title=$5 body=$6
  if ! has_branch "$base"; then
    start_at main
    "${fixture}_tree" base "$work"
    commit "test($fixture): add the files $branch changes"
    push "$base"
    say "pushed $base"
  fi
  if ! has_branch "$branch"; then
    start_at "$base"
    "${fixture}_tree" head "$work"
    commit "$subject"
    push "$branch"
    say "pushed $branch"
  fi
  PR=$(pr_number "$branch")
  if [[ -z $PR ]]; then open_pr "$branch" "$base" "$title" "$body"; fi
}

# Builds what's missing of #17, whose PR only adds files: <branch> from main, then the PR into
# main. Sets PR.
added_tree_setup() {
  local fixture=$1 branch=$2 subject=$3 title=$4 body=$5
  if ! has_branch "$branch"; then
    start_at main
    "${fixture}_tree" "$work"
    commit "$subject"
    push "$branch"
    say "pushed $branch"
  fi
  PR=$(pr_number "$branch")
  if [[ -z $PR ]]; then open_pr "$branch" main "$title" "$body"; fi
}

# #17 was opened by hand before this script made it, with the same files and title.
images_setup() {
  local body
  # read, not $(cat <<…), which bash 3.2 can't parse when the text has an apostrophe.
  IFS= read -r -d '' body <<'EOF' || true
Fixture for checking Lo's image previews on screen. Not for merging.

- `e5/desktop-picture.svg`, `e5/local-image.svg`, `e5/dev-zero.svg`: each `<image>` points at a file on the reviewer's disk (a macOS desktop picture, the default wallpaper through a symlink and a relative path, and `/dev/zero`), as `href`, `xlink:href` and `file://` URLs. Only each SVG's frame and coloured dot should draw.
- `e5/pattern-bomb.svg`: 316 bytes, a `<pattern>` with a 20000 × 20000 tile. The preview should fail with "couldn't preview this image" and the app should stay responsive.
- `e5/small.png`: a 48 × 48 gradient that should preview normally.

Made by `script/e2e/fixtures.sh` in scornz/lo.
EOF
  added_tree_setup images e5/images $'test(e5): add image preview safety fixtures\n\nSVGs whose <image> points at local files and /dev/zero, a 316-byte pattern bomb with a 20000x20000 tile, and a small PNG.' \
    "test(e5): image preview safety fixtures" "$body"
}

e6_setup() {
  changed_tree_setup e6 e6/base e6/code "test(e6): add and change code in four languages" \
    "E6: code" "Fixture E6 (code, to trust): Rust, TypeScript, Python and Go under \`e6/\`, for Lo's code navigation, language servers and trust. \`e6/rust/src/render/parse.rs\` adds a second \`parse\`, colliding with \`e6/rust/src/parse.rs\`'s; \`lib.rs\` imports serde, which is never installed; and \`summarize\` becomes \`describe\`, so the old name still resolves on the left. The PR calls functions in files it changes and in files it doesn't. It targets \`e6/base\`, which adds the files it changes to \`main\`. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
}

# E8 targets e8/base, which holds only what main held when E8 was made, so no merge of E8 can put
# its baits on main. Unlike the other fixtures, setup keeps E8 up to date, because its baits follow
# Lo's hardening: when trees.sh changes them, it pushes the change to e8/hostile as a new commit, a
# normal push, and brings the PR's base and description into line.
e8_setup() {
  local body at info state base current
  IFS= read -r -d '' body <<'EOF' || true
**A test fixture for Lo's trust handling, not code to run or merge.**

Its files try to run code, or steer a language server, when the pull request is checked out and opened:

- `.cargo/config.toml` sets a `rustc-wrapper`;
- `.zed/settings.json` points rust-analyzer, gopls, vtsls and basedpyright at another binary;
- `node_modules/typescript/lib/tsserver.js` is a workspace TypeScript server;
- `.venv/`, which `pyrightconfig.json` names, is a virtual environment whose interpreter is a script;
- `e8/pinned/rust-toolchain.toml` and `go.mod` name toolchains that don't exist.

The only script any of them would run is `e8/evil.sh`, which creates one empty marker file, `$TMPDIR/lo-e8-<name>`, and nothing else: no network, no deletes, no reads. Lo passes while no marker appears. The toolchain files can only make rustup or go try a download, so they leave no marker; `e8/README.md` lists each file and its marker, and says how a check sees the toolchains.

It targets `e8/base`, which holds only what `main` held when E8 was made, so merging it can't put these files on `main`.

Made by `script/e2e/fixtures.sh` in scornz/lo.
EOF
  body=${body%$'\n'}
  if ! has_branch e8/base; then
    # An E8 made before e8/base existed forked from main, so e8/base starts at that fork, and the
    # PR's diff stays the same.
    if has_branch e8/hostile; then
      at=$(g merge-base refs/remotes/origin/main refs/remotes/origin/e8/hostile)
      g checkout --quiet --detach "$at"
    else
      start_at main
    fi
    push e8/base
    say "pushed e8/base"
  fi
  if has_branch e8/hostile; then
    start_at e8/hostile
    e8_tree "$work"
    g add --all
    if ! g diff --cached --quiet; then
      commit "test(e8): bring the hostile files up to date"
      push e8/hostile
      say "updated e8/hostile"
    fi
  else
    start_at e8/base
    e8_tree "$work"
    commit "test(e8): add files that try to run code when opened"
    push e8/hostile
    say "pushed e8/hostile"
  fi
  PR=$(pr_number e8/hostile)
  if [[ -z $PR ]]; then
    open_pr e8/hostile e8/base "E8: hostile files" "$body"
    return
  fi
  info=$(gh_api "repos/$REPO/pulls/$PR")
  state=$(jq -r 'if .merged_at then "merged" else .state end' <<<"$info")
  base=$(jq -r .base.ref <<<"$info")
  current=$(jq -r '.body // ""' <<<"$info")
  if [[ $state == merged && $base != e8/base ]]; then
    die "#$PR merged E8's baits into $base; take them out of $base by hand"
  fi
  [[ $state == open ]] || return 0
  if [[ $base != e8/base ]]; then
    gh_api -X PATCH "repos/$REPO/pulls/$PR" -f base=e8/base --silent
    CHANGED=$((CHANGED + 1))
    say "retargeted #$PR from $base to e8/base"
  fi
  if [[ $current != "$body" ]]; then
    gh_api -X PATCH "repos/$REPO/pulls/$PR" -f body="$body" --silent
    CHANGED=$((CHANGED + 1))
    say "updated #$PR's description"
  fi
}

# The number of the one line of <path>, at <branch>'s tip, that reads <text>.
line_of() {
  local n
  n=$(g show "refs/remotes/origin/$1:$2" | TEXT=$3 awk '$0 == ENVIRON["TEXT"] { print NR }') ||
    return 1
  if [[ ! $n =~ ^[0-9]+$ ]]; then
    say "$2 on $1 doesn't have exactly one line reading: $3"
    return 1
  fi
  echo "$n"
}

# Posts rozed-bot's COMMENT review on E9's PR <n>, at its head, with a thread on a moved line of
# e9/api/names.ts and one on a line of e9/api/user.ts only the old side has.
e9_review() {
  local pr=$1 head moved old request
  head=$(pr_head "$pr") || return 1
  moved=$(line_of e9/moves e9/api/names.ts "$E9_MOVED_LINE") || return 1
  old=$(line_of e9/base e9/api/user.ts "$E9_OLD_LINE") || return 1
  request=$(jq -n --arg head "$head" --arg body "$E9_REVIEW" \
    --argjson moved "$moved" --arg moved_body "$E9_MOVED_COMMENT" \
    --argjson old "$old" --arg old_body "$E9_OLD_COMMENT" '{
      commit_id: $head, event: "COMMENT", body: $body,
      comments: [
        {path: "e9/api/names.ts", side: "RIGHT", line: $moved, body: $moved_body},
        {path: "e9/api/user.ts", side: "LEFT", line: $old, body: $old_body}
      ]}') || return 1
  gh_api -X POST "repos/$REPO/pulls/$pr/reviews" --input - --silent <<<"$request"
}

# GitHub may not have worked out a new PR's diff yet, and refuses comments on it until then. A
# failed POST may still have posted the review, so it looks before trying again.
e9_review_once() {
  local count
  e9_review "$1" && return 0
  count=$(review_count "$1" "$E9_REVIEW") || return 1
  ((count > 0))
}

e9_setup() {
  local pr count
  changed_tree_setup e9 e9/base e9/moves "test(e9): move code and change its syntax" \
    "E9: moves and syntax" "Fixture E9 (moves and syntax), for Lo's moved code and syntax-aware view. Moves: \`fullName\` from \`e9/api/user.ts\` into the \`Names\` class in \`e9/api/names.ts\`, re-indented with its return line edited; \`checksum\` up \`e9/lib.rs\`; and two lines down \`e9/short.py\`, too few to count. Syntax: a renamed variable (\`e9/syntax/rename.rs\`), reordered arguments (\`args.ts\`), a block wrapped in a new \`if\` (\`wrap.py\`) and reordered JSON keys (\`config.json\`). Fallbacks: a parse error (\`e9/broken.ts\`) and a 1.2 MB file (\`e9/large.ts\`). rozed-bot's review has a thread on a moved line and one on a line only the old side has. It targets \`e9/base\`, which adds the files it changes to \`main\`. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
  pr=$PR
  count=$(review_count "$pr" "$E9_REVIEW")
  if ((count == 0)); then
    retry 10 e9_review_once "$pr" || die "couldn't post E9's review on #$pr"
    CHANGED=$((CHANGED + 1))
    say "reviewed #$pr"
  fi
}

e10_setup() {
  changed_tree_setup e10 e10/base e10/large "test(e10): change 300 files" "E10: large" \
    "Fixture E10 (large), for Lo's performance budgets: a seeded, deterministic PR of 300 files and about 25,000 changed lines, in TypeScript, Rust, Python, Go and TOML. It changes 254 files, adds 30 and deletes 16, moves functions within and between files, adds a 6,000-line file (\`e10/src/big/catalog.ts\`) and changes a lockfile (\`e10/package-lock.json\`). It targets \`e10/base\`, which adds the files it changes to \`main\`. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
}

e12_setup() {
  changed_tree_setup e12 e12/base e12/languages "test(e12): change a file in each extension language" \
    "E12: extension languages" "Fixture E12 (extension languages), for the languages Lo bundles as extensions: one file each in Swift, Kotlin, Terraform, Ruby, Java, C#, PHP, Elixir, TOML, HTML, a Dockerfile and a Makefile, each changed, so both sides need highlighting. None is part of a project a language server would build. It targets \`e12/base\`, which adds the files it changes to \`main\`. Made by \`script/e2e/fixtures.sh\` in scornz/lo."
}

# E3 goes first: its merge moves main, so a new E1's base then moves only under rebase-r5.
cmd_setup() {
  clone
  e3_setup
  e1_setup
  e2_setup
  images_setup
  e6_setup
  e8_setup
  e9_setup
  e10_setup
  e12_setup
  if ((CHANGED == 0)); then say "Nothing to change: every fixture exists."; fi
}

# --- status ------------------------------------------------------------------------------------

# E1's revisions as pushed: r1 to r4 are the PR's commits up to the head the rebase replaced, r5 is
# the rebase's result, and later ones are new commits on top of it.
e1_revisions() {
  local pr=$1 pushes=$2 head before pre post='{}' compare
  head=$(pr_head "$pr") || return 1
  before=$(jq -r 'if length > 0 then .[0].before else "" end' <<<"$pushes") || return 1
  [[ -n $before ]] || before=$head
  compare=$(gh_api "repos/$REPO/compare/main...$before") || return 1
  pre=$(jq "$E1_REVISIONS" <<<"$compare") || return 1
  if [[ $before != "$head" ]]; then
    compare=$(gh_api "repos/$REPO/compare/main...$head") || return 1
    post=$(jq "$E1_REVISIONS" <<<"$compare") || return 1
  fi
  jq -n --argjson pre "$pre" --argjson post "$post" --argjson pushes "$pushes" '
    $pre + (if ($pushes | length) > 0 then {r5: $pushes[0].after} else {} end)
    + ($post | with_entries(select(.key as $k | $pre | has($k) | not)))'
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum; else shasum -a 256; fi | awk '{ print $1 }'
}

# Maps each path on stdin, one a line, to the SHA-256 of the path, which GitHub's links to a file
# in a PR's diff use (#diff-<hash>), as a JSON object.
path_hashes() {
  local path hash
  while IFS= read -r path; do
    hash=$(printf '%s' "$path" | sha256) || return 1
    printf '%s\t%s\n' "$path" "$hash"
  done | jq -R -s 'split("\n") | map(select(. != "") | split("\t") | {(.[0]): .[1]}) | add // {}'
}

# PR <n>'s changed files, as [{path, status, additions, deletions, hash}].
pr_files() {
  local files hashes
  files=$(gh_api --paginate "repos/$REPO/pulls/$1/files?per_page=100") || return 1
  files=$(jq -s 'add // [] | map({path: .filename, status, additions, deletions})' <<<"$files") ||
    return 1
  hashes=$(jq -r '.[].path' <<<"$files" | path_hashes) || return 1
  jq --argjson hashes "$hashes" 'map(. + {hash: $hashes[.path]})' <<<"$files"
}

# The PR from <branch>, from status_json's $prs and $heads: its number, branch, base, head and
# state, and its base branch's head. With `files`, also its changed files; with `size`, only how
# many, and its added and deleted lines. null when there's no PR.
fixture_json() {
  local pr extra
  pr=$(jq --arg b "$1" 'first(.[] | select(.branch == $b) | .pr) // empty' <<<"$prs") || return 1
  if [[ -z $pr ]]; then
    echo null
    return
  fi
  case $2 in
    files) extra=$(pr_files "$pr") && extra=$(jq '{files: .}' <<<"$extra") ;;
    size)
      extra=$(gh_api "repos/$REPO/pulls/$pr" \
        --jq '{size: {files: .changed_files, additions, deletions}}')
      ;;
    *) die "fixture_json takes files or size" ;;
  esac || return 1
  jq --arg b "$1" --argjson heads "$heads" --argjson extra "$extra" '
    first(.[] | select(.branch == $b)) | del(.merge_commit) + {base_head: $heads[.base]} + $extra' \
    <<<"$prs"
}

# rozed-bot's review on E9's PR <n>, with its threads' first comments, or null.
e9_review_json() {
  local review comments
  review=$(gh_api "repos/$REPO/pulls/$1/reviews?per_page=100") || return 1
  review=$(jq --arg bot "$BOT" --arg body "$E9_REVIEW" 'first(.[]
    | select(.user.login == $bot and .body == $body) | {id, commit: .commit_id}) // null' \
    <<<"$review") || return 1
  if [[ $review == null ]]; then
    echo null
    return
  fi
  comments=$(gh_api --paginate "repos/$REPO/pulls/$1/comments?per_page=100") || return 1
  jq -s --argjson review "$review" 'add // [] | $review + {comments: [.[]
    | select(.pull_request_review_id == $review.id) | {
      thread: (if .side == "LEFT" then "old_side_only" else "moved" end),
      id, node_id, path, side, line}]}' <<<"$comments"
}

status_json() {
  local prs heads branches main max e1=null e1_branch e1_pr pushes review revisions stack=null s1
  local e6 e8 e9 e10 e12 images
  # Only rozed-bot's PRs from rozed-dev/e2e itself: anyone can open a PR from a fork's e2/d.
  prs=$(gh_api --paginate "repos/$REPO/pulls?state=all&per_page=100")
  prs=$(jq -s --arg bot "$BOT" --arg repo "$REPO" 'add // []
    | map(select(.user.login == $bot and .head.repo.full_name == $repo) | {
      pr: .number, branch: .head.ref, base: .base.ref, head: .head.sha,
      state: (if .merged_at then "merged" else .state end),
      merge_commit: (if .merged_at then .merge_commit_sha else null end)})' <<<"$prs")
  heads=$(gh_api --paginate "repos/$REPO/branches?per_page=100" --jq '.[] | {(.name): .commit.sha}')
  heads=$(jq -s 'add // {}' <<<"$heads")
  branches=$(jq -r 'keys[]' <<<"$heads")
  main=$(gh_api "repos/$REPO/branches/main" --jq .commit.sha)

  max=$(e1_max <<<"$branches")
  if ((max > 0)); then
    e1_branch=$(e1_branch_name "$max")
    e1_pr=$(jq --arg b "$e1_branch" 'first(.[] | select(.branch == $b) | .pr) // empty' <<<"$prs")
    pushes='[]' review=null revisions='{}'
    if [[ -n $e1_pr ]]; then
      pushes=$(force_pushes "$e1_pr")
      review=$(gh_api "repos/$REPO/pulls/$e1_pr/reviews?per_page=100")
      review=$(jq --arg bot "$BOT" --arg body "$E1_REVIEW" 'first(.[]
        | select(.user.login == $bot and .body == $body) | {id, commit: .commit_id}) // null' \
        <<<"$review")
      revisions=$(e1_revisions "$e1_pr" "$pushes")
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
  if [[ -n $s1 ]]; then
    stack=$(gh_api "repos/$REPO/stacks?pull_request=$s1")
    stack=$(jq 'first(.[] | {number, prs: [.pull_requests[].number]}) // null' <<<"$stack")
  fi

  images=$(fixture_json e5/images files)
  e6=$(fixture_json e6/code files)
  e8=$(fixture_json e8/hostile files)
  e9=$(fixture_json e9/moves files)
  if [[ $e9 != null ]]; then
    review=$(jq .pr <<<"$e9")
    review=$(e9_review_json "$review")
    e9=$(jq --argjson review "$review" '. + {review: $review}' <<<"$e9")
  fi
  e10=$(fixture_json e10/large size)
  e12=$(fixture_json e12/languages files)

  jq --arg main "$main" --argjson e1 "$e1" --argjson stack "$stack" --arg layers "$E2_LAYERS" \
    --argjson e6 "$e6" --argjson e8 "$e8" --argjson e9 "$e9" --argjson e10 "$e10" \
    --argjson e12 "$e12" --argjson images "$images" '
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
      },
      e6: $e6,
      e8: $e8,
      e9: $e9,
      e10: $e10,
      e12: $e12,
      images: $images
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
    (if .e3.y then "      retargeted: \(.e3.y.retargeted)" else empty end),
    "E6 code",
    (.e6 | row("e6")),
    "E8 hostile files",
    (.e8 | row("e8")),
    "E9 moves and syntax",
    (.e9 | row("e9")),
    (if .e9 == null then empty
     elif .e9.review then "      review \(.e9.review.id) on \(.e9.review.commit | short)",
       (.e9.review.comments[] | "      \(.thread) thread #\(.id) on \(.side) \(.path):\(.line)")
     else "      no review" end),
    "E10 large",
    (.e10 | row("e10")),
    (if .e10 then "      \(.e10.size.files) files, +\(.e10.size.additions) -\(.e10.size.deletions)" else empty end),
    "E12 extension languages",
    (.e12 | row("e12")),
    "#17 images",
    (.images | row("images"))'
}

cmd_status() {
  case ${1:-} in
    --json) status_json ;;
    '')
      # Through a file, not $(…), so `set -e` still applies inside status_json.
      status_json >"$tmp/status.json"
      status_text <"$tmp/status.json"
      ;;
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
    setup | push-r4 | rebase-r5 | push-r6 | reset-e1)
      [[ $# -eq 0 ]] || die "$cmd takes no arguments"
      ;;
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
    push-r4)
      clone
      cmd_push_r4
      ;;
    rebase-r5)
      clone
      cmd_rebase_r5
      ;;
    push-r6)
      clone
      cmd_push_r6
      ;;
    reset-e1)
      clone
      cmd_reset_e1
      ;;
    *) die "unknown command $cmd" ;;
  esac
}

main "$@"
