#!/usr/bin/env bash
# Renames the project's code names from rozed to lo, then lists every line that still mentions
# rozed, for a hand pass. A later PR deletes this script, and the CI job that runs --check, once
# every open branch has run it.
#
# Usage: script/rename-to-lo.sh [--list | --check]
#
#   --list   change nothing; only list the lines that still mention rozed
#   --check  change nothing; list them, and fail if there are any (CI runs this)
#
# Only names that are code change here, all to lowercase lo: the crate, package and binary
# (crates/rozed -> crates/lo), identifiers (RozedDb -> LoDb, restart_rozed -> restart_lo), git
# refs (refs/rozed/ -> refs/lo/), environment variables (ROZED_* -> LO_*), the URL schemes
# (rozed:// -> lo://, rozed-dev:// -> lo-dev://), the folders (~/.config/lo, and
# ~/Library/Application Support/Lo, ~/Library/Caches/Lo and ~/Library/Logs/Lo, capitalised as
# Zed's are), file names, any other name with a rozed- prefix (rozed-last-used -> lo-last-used),
# the user agent and the repository (scornz/rozed -> scornz/lo).
#
# Text a person reads isn't changed here: menus, window titles, messages and docs say "Lo", with
# a capital L, even mid-sentence, and that's for a person to judge line by line. The list at the
# end shows what's left. The rules can't tell code from prose, though, so the script also prints
# every Markdown line it changed. Check those, and the diff, for a hyphenated word it lowercased,
# such as rozed-specific, which should read Lo-specific.
#
# Kept as they are: the test org rozed-dev and its secrets folder ~/.config/rozed-dev/, the bot
# rozed-bot (its token variable ROZED_BOT_TOKEN, which ~/.config/rozed-dev/bot.env sets, and its
# email address), the Zed fork's branch rozed/v1.27.0, this Mac's ~/src/rozed and
# ~/src/rozed-worktrees folders, the Zed worktrees named after a patch branch, such as
# ~/src/zed-worktrees/rozed-v1.27.0, and text already on GitHub: recorded GitHub data under
# tests/fixtures/, which the script lists but never rewrites, two tests that quote
# rozed-dev/playground's pull requests, and the bot's review that script/e2e/fixtures.sh looks up.
# A line that says rename:keep, such as a Markdown line ending in <!-- rename:keep -->, is kept
# too: use it for a line that must name rozed, such as one describing the rename.
#
# Running it again changes nothing more. On a branch that predates the rename, merge origin/main
# first, then run it: the merge brings main's renamed lines, git moves files the branch added in
# crates/rozed/ to crates/lo/, and the script renames what's left, the branch's own lines. Run
# before the merge, it would conflict wherever main's hand pass changed a line it renamed. On a
# branch that changes only docs, run --list and edit the lines by hand instead: docs about the
# rename name both rozed and lo, and the rewrite can't tell those lines apart.
set -euo pipefail

usage() {
  sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'
}

mode=rewrite
case ${1:-} in
  '') ;;
  --list) mode=list ;;
  --check) mode=check ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    echo "rename-to-lo: unknown argument $1" >&2
    usage >&2
    exit 2
    ;;
esac

cd "$(git rev-parse --show-toplevel)"
readonly self=script/rename-to-lo.sh
markdown=
trap '[[ -z $markdown ]] || rm -f "$markdown"' EXIT

# Names that keep "rozed", in any case. Every rule runs with them swapped out, and the listing
# ignores them. rozed-dev:// is the debug builds' old token prefix, so it isn't kept.
readonly KEPT='rozed-dev(?!://)|rozed-bot|rozed/v1\.27\.0(?![\w.-])|ROZED_BOT_TOKEN|rozed-test\@scornz\.com|(?:~/src/)?rozed-worktrees|~/src/rozed(?![\w.-])|(?:zed-worktrees/)?rozed-v(?:\d[\d.]*(?:-\d+)?|<version>)|rozed (?:stack fixture|test fixtures|marks this revision)'

# What a line says to be kept whole.
readonly KEEP_MARKER='rename:keep'

# The code-name renames, as one Perl substitution pass over a line or a path. Kept names are
# swapped out first and restored after, so no rule can touch them.
# shellcheck disable=SC2016 # Perl, not shell
readonly RENAME='
  unless (/'"$KEEP_MARKER"'/) {
    my @kept;
    s{('"$KEPT"')}{push @kept, $1; "\x00" . $#kept . "\x00"}gie;

    s{rozed-dev://}{lo-dev://}g;
    s{rozed://}{lo://}g;
    s{\brefs/rozed\b}{refs/lo}g;
    s{(?<![A-Z])ROZED(?=[_-])}{LO}g;
    s{\.config/rozed(?![\w-])}{.config/lo}g;
    s{Application Support/rozed(?![\w-])}{Application Support/Lo}g;
    s{Library/Caches/rozed(?![\w-])}{Library/Caches/Lo}g;
    s{Library/Logs/rozed(?![\w-])}{Library/Logs/Lo}g;
    s{Library/Logs/Lo/rozed\.log}{Library/Logs/Lo/Lo.log}g;
    s{\bcrates/rozed(?![\w-])}{crates/lo}g;
    s{(--?(?:p|package|bin)[ =])rozed(?![\w-])}{${1}lo}g;
    s{\b(target/(?:debug|release)/)rozed(?![\w-])}{${1}lo}g;
    s{\bscornz/rozed(?![\w-])}{scornz/lo}g;
    s{\bRozed(?=[A-Z])}{Lo}g;
    s{(?<=_)rozeds(?![A-Za-z0-9])}{los}g;
    s{(?<![A-Za-z0-9])rozeds(?=_)}{los}g;
    s{(?<=_)rozed(?![A-Za-z0-9])}{lo}g;
    s{(?<![A-Za-z0-9])rozed(?=_)}{lo}g;
    s{(?<![\w-])rozed\.(sqlite|scm|invalid)\b}{lo.$1}g;
    s{(?<![A-Za-z0-9])rozed-(?=[A-Za-z0-9])}{lo-}g;
    s{(?<=["\x27`])rozed/(?!v[\d<])}{lo/}g;
    s{(?<=[=,])rozed(?==)}{lo}g;
    s{\brozed::(?=[a-z_])}{lo::}g;
    s{\b(usage|example): rozed\b}{$1: lo}g;
    s{(?<=`)rozed(?=`| --?\w| <| https?://)}{lo}g;

    s{\x00(\d+)\x00}{$kept[$1]}g;
  }
'

# Recorded GitHub data: what GitHub said keeps its wording, so the rewrite leaves it alone.
is_recorded() {
  [[ $1 == */tests/fixtures/* ]]
}

# Renames tracked files whose paths hold a code name, such as crates/rozed/ and rozed.scm.
rename_paths() {
  local old new
  while IFS= read -r -d '' old; do
    ! is_recorded "$old" || continue
    new=$(printf '%s\n' "$old" | perl -pe "$RENAME")
    [[ $new != "$old" ]] || continue
    if [[ -e $new ]]; then
      echo "rename-to-lo: both $old and $new exist; merge them by hand" >&2
      continue
    fi
    mkdir -p "$(dirname "$new")"
    git mv "$old" "$new"
    echo "renamed $old -> $new" >&2
  done < <(git ls-files -z -- ':(icase)*rozed*')
  # Folders the moves emptied.
  if [[ -d crates/rozed ]]; then
    find crates/rozed -depth -type d -empty -delete
  fi
}

# Applies the renames to every tracked text file that mentions rozed, except this script and
# recorded GitHub data. Then it formats the Rust files that changed, since shorter names can let
# rustfmt join lines, and prints each Markdown line it changed.
rename_contents() {
  local file before
  local changed=()
  markdown=$(mktemp)
  while IFS= read -r -d '' file; do
    [[ $file != "$self" && -f $file && ! -L $file ]] || continue
    ! is_recorded "$file" || continue
    before=$(cksum < "$file")
    case $file in
      *.md)
        # shellcheck disable=SC2016 # Perl, not shell
        perl -pi -e 'my $was = $_; '"$RENAME"'
          print STDERR "$ARGV:$.:$_" if $_ ne $was;' "$file" 2>> "$markdown"
        ;;
      *) perl -pi -e "$RENAME" "$file" ;;
    esac
    case $file in
      Cargo.toml | */Cargo.toml) perl -pi -e 's/^name = "rozed"$/name = "lo"/' "$file" ;;
      package.json) perl -pi -e 's/^(  "name": )"rozed",$/$1"lo",/' "$file" ;;
      *) ;;
    esac
    if [[ $file == *.rs && $(cksum < "$file") != "$before" ]]; then
      changed+=("$file")
    fi
  done < <(git grep -z -l -I -i rozed || true)
  if [[ -s $markdown ]]; then
    echo "rename-to-lo: $(wc -l < "$markdown" | tr -d ' ') Markdown lines changed; read each:" >&2
    cat "$markdown"
  fi
  [[ ${#changed[@]} -gt 0 ]] || return 0
  if command -v rustfmt > /dev/null; then
    rustfmt "${changed[@]}" || echo "rename-to-lo: rustfmt failed; run cargo fmt once it builds" >&2
  else
    echo "rename-to-lo: no rustfmt on PATH; run cargo fmt" >&2
  fi
}

# Renames the package in Cargo.lock and moves its entry to where cargo sorts it: by name, byte by
# byte. A stable sort by name keeps the order of entries that share one, which cargo already set.
rename_lock() {
  [[ -f Cargo.lock ]] || return 0
  grep -q '^name = "rozed"$' Cargo.lock || return 0
  # shellcheck disable=SC2016 # Perl, not shell
  perl -0777 -pi -e '
    s/^name = "rozed"$/name = "lo"/m;
    s/^ "rozed",$/ "lo",/mg;
    my ($head, @blocks) = split /\n(?=\[\[)/;
    my @packages = grep { /^\[\[package\]\]/ } @blocks;
    my @rest = grep { !/^\[\[package\]\]/ } @blocks;
    my %name = map { $_ => /^name = "([^"]*)"$/m ? $1 : "" } @packages;
    my $i = 0;
    my %index = map { $_ => $i++ } @packages;
    @packages = sort { $name{$a} cmp $name{$b} or $index{$a} <=> $index{$b} } @packages;
    $_ = join "\n", $head, @packages, @rest;
  ' Cargo.lock
  echo "renamed the rozed package in Cargo.lock" >&2
}

# Lists the lines that still mention rozed outside the kept names, then the recorded GitHub data
# that does, which keeps its wording. Returns 1 when any line of the first kind is left.
list_remaining() {
  # shellcheck disable=SC2016 # Perl, not shell
  { git grep -n -I -i rozed -- . ":(exclude)$self" || true; } |
    perl -ne '
      BEGIN { $| = 1 }
      my ($path, $where, $text) = /^(([^:]*):\d+):(.*)$/s ? ($2, $1, $3) : next;
      next if $text =~ /'"$KEEP_MARKER"'/;
      (my $rest = $text) =~ s{'"$KEPT"'}{}gi;
      next unless $rest =~ /rozed/i;
      if ($path =~ m{/tests/fixtures/}) { push @recorded, "$where:$text"; next }
      print "$where:$text";
      $n++;
      END {
        printf STDERR "rename-to-lo: %d lines still mention rozed outside the kept names.\n", $n // 0;
        if (@recorded) {
          printf STDERR "rename-to-lo: %d lines of recorded GitHub data keep their wording:\n",
            scalar @recorded;
          print @recorded;
        }
        $? = $n ? 1 : 0;
      }
    '
}

if [[ $mode == rewrite ]]; then
  rename_paths
  rename_contents
  rename_lock
fi
if list_remaining; then
  exit 0
elif [[ $mode == check ]]; then
  echo "rename-to-lo: rename them: run this script, then fix what it lists by hand." >&2
  exit 1
fi
