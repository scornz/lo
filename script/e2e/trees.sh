# shellcheck shell=bash
# The files of fixtures E6, E8, E9, E10, E11, E12 and #17, which fixtures.sh sources and pushes.
#
# Each <fixture>_tree function writes one side of a fixture into the folder of a checkout,
# replacing what's there:
#
#   e6_tree, e9_tree, e10_tree, e11_tree, e12_tree <base | head> <folder>
#   e8_tree, images_tree <folder>      (their PRs only add files, so they have no base side)
#
# Nothing here reads the network or the token, and every file comes out byte for byte the same
# each time, on any machine, so a tree can be written and looked at anywhere:
#
#   (. script/e2e/trees.sh && e9_tree head "$(mktemp -d)")
#
# E9's 1.2 MB file and all of E10 come from awk with a Park-Miller generator, x * 16807 mod
# 2^31 - 1, which stays exact in awk's doubles, so every awk makes the same files.

# Writes stdin to <path>, making its folder.
put() {
  mkdir -p "$(dirname "$1")"
  cat >"$1"
}

# Fails unless <side> is base or head.
check_side() {
  case $1 in
    base | head) ;;
    *)
      echo "trees.sh: a side is base or head, not $1" >&2
      return 1
      ;;
  esac
}

# --- E6: code, in four languages ----------------------------------------------------------------

# Rust, TypeScript, Python and Go. The head side:
# - adds e6/rust/src/render/parse.rs, whose `parse` collides with e6/rust/src/parse.rs's;
# - imports serde in lib.rs, which Cargo.toml never declares, so it's never installed;
# - renames `summarize` to `describe` in lib.rs, so the left side's `summarize` still resolves;
# - calls functions in files it changes (render/parse.rs, user.ts, util.py, util.go) and in files
#   it doesn't (parse.rs's `count`, names.ts's `initials`).
e6_tree() {
  local side=$1 dir=${2:?}/e6
  check_side "$side" || return 1
  rm -rf "$dir"

  put "$dir/rust/Cargo.toml" <<'EOF'
[package]
name = "e6"
version = "0.1.0"
edition = "2021"

# src/lib.rs uses serde, which is never declared, so it's never installed.
[dependencies]
EOF

  put "$dir/rust/src/parse.rs" <<'EOF'
//! Splits text into words.

/// Splits `text` into words, on whitespace.
pub fn parse(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Counts the words in `text`.
pub fn count(text: &str) -> usize {
    parse(text).len()
}
EOF

  if [[ $side == base ]]; then
    put "$dir/rust/src/lib.rs" <<'EOF'
//! Fixture E6: Rust code for Lo's code navigation.

pub mod parse;

/// Summarises `text` by its word count.
pub fn summarize(text: &str) -> String {
    format!("{} words", parse::count(text))
}

/// Summarises a document's body.
pub fn run(body: &str) -> String {
    summarize(body)
}
EOF
  else
    put "$dir/rust/src/lib.rs" <<'EOF'
//! Fixture E6: Rust code for Lo's code navigation.

use serde::Deserialize;

pub mod parse;
pub mod render {
    pub mod parse;
}

/// A document to describe. serde is never installed, so `Deserialize` never resolves.
#[derive(Deserialize)]
pub struct Document {
    pub title: String,
    pub body: String,
}

/// Describes `text` by its word count. It was `summarize`.
pub fn describe(text: &str) -> String {
    format!("{} words", parse::count(text))
}

/// Describes a document: its title as HTML, then its body.
pub fn run(doc: &Document) -> String {
    let title = render::parse::parse(&doc.title);
    let words = parse::parse(&doc.body);
    format!("{title} {} (first word: {:?})", describe(&doc.body), words.first())
}
EOF
    put "$dir/rust/src/render/parse.rs" <<'EOF'
//! Renders a title as HTML. Its `parse` shares a name with `crate::parse::parse`.

/// Parses `title` into an HTML heading, escaping angle brackets.
pub fn parse(title: &str) -> String {
    let escaped = title.replace('<', "&lt;").replace('>', "&gt;");
    format!("<h1>{escaped}</h1>")
}
EOF
  fi

  put "$dir/ts/src/names.ts" <<'EOF'
// Fixture E6: name helpers the pull request calls but doesn't change.

export function initials(name: string): string {
  return name
    .split(/\s+/)
    .filter((part) => part.length > 0)
    .map((part) => part[0].toUpperCase())
    .join("");
}

export function capitalize(word: string): string {
  return word.charAt(0).toUpperCase() + word.slice(1);
}
EOF

  if [[ $side == base ]]; then
    put "$dir/ts/src/user.ts" <<'EOF'
import { capitalize } from "./names";

export interface User {
  first: string;
  last: string;
}

export function fullName(user: User): string {
  return `${capitalize(user.first)} ${capitalize(user.last)}`;
}
EOF
    put "$dir/ts/src/index.ts" <<'EOF'
import { fullName, type User } from "./user";

export function greet(user: User): string {
  return `Hello, ${fullName(user)}!`;
}
EOF
  else
    put "$dir/ts/src/user.ts" <<'EOF'
import { capitalize } from "./names";

export interface User {
  first: string;
  last: string;
  admin?: boolean;
}

export function fullName(user: User): string {
  return `${capitalize(user.first)} ${capitalize(user.last)}`;
}

export function isAdmin(user: User): boolean {
  return user.admin === true;
}
EOF
    put "$dir/ts/src/index.ts" <<'EOF'
import { initials } from "./names";
import { fullName, isAdmin, type User } from "./user";

export function greet(user: User): string {
  const badge = isAdmin(user) ? " (admin)" : "";
  return `Hello, ${fullName(user)}${badge}!`;
}

export function monogram(user: User): string {
  return initials(fullName(user));
}
EOF
  fi

  if [[ $side == base ]]; then
    put "$dir/py/util.py" <<'EOF'
"""Fixture E6: Python helpers."""


def slugify(text):
    return "-".join(text.lower().split())
EOF
    put "$dir/py/app.py" <<'EOF'
"""Fixture E6: a Python app that uses util."""

from util import slugify


def page_path(title):
    return "/pages/" + slugify(title)
EOF
  else
    put "$dir/py/util.py" <<'EOF'
"""Fixture E6: Python helpers."""


def slugify(text):
    return "-".join(text.lower().split())


def truncate(text, limit):
    return text if len(text) <= limit else text[: limit - 3] + "..."
EOF
    put "$dir/py/app.py" <<'EOF'
"""Fixture E6: a Python app that uses util."""

from util import slugify, truncate


def page_path(title):
    return "/pages/" + slugify(truncate(title, 40))


def page_link(title):
    return f'<a href="{page_path(title)}">{title}</a>'
EOF
  fi

  put "$dir/go/go.mod" <<'EOF'
module example.com/e6

go 1.21
EOF

  if [[ $side == base ]]; then
    put "$dir/go/main.go" <<'EOF'
// Fixture E6: a Go program.
package main

import "fmt"

func main() {
	fmt.Println(greeting("world"))
}

func greeting(name string) string {
	return "Hello, " + name
}
EOF
  else
    put "$dir/go/main.go" <<'EOF'
// Fixture E6: a Go program.
package main

import "fmt"

func main() {
	fmt.Println(greeting(shout("world")))
}

func greeting(name string) string {
	return "Hello, " + name
}
EOF
    put "$dir/go/util.go" <<'EOF'
package main

import "strings"

// shout upper-cases name and adds an exclamation mark.
func shout(name string) string {
	return strings.ToUpper(name) + "!"
}
EOF
  fi
}

# --- E8: hostile files ----------------------------------------------------------------------------

# Files that try to run code, or steer a language server, when the PR is checked out. Each one that
# can run something runs only e8/evil.sh, which creates an empty marker file, $TMPDIR/lo-e8-<name>:
# no network, no deletes and no reads. The two toolchain files can only make rustup or go try to
# download a toolchain that doesn't exist, so they leave no marker; e8/README.md says how a check
# sees them. The config files sit where each tool looks for them, mostly at the repository's root,
# and e8/ holds a small project in each language, so each server has something to open.
e8_tree() {
  local root=${1:?}
  rm -rf "$root/e8" "$root/.cargo" "$root/.zed" "$root/.venv" "$root/node_modules"
  rm -f "$root/rust-toolchain.toml" "$root/pyrightconfig.json" "$root/go.mod"

  put "$root/e8/README.md" <<'EOF'
# Fixture E8: hostile files

A test fixture for Lo's trust handling, not real code. The files in this pull request try to run
code, or to steer a language server, when the repository is checked out and opened. Each one that
can run something runs only `e8/evil.sh`, which creates an empty marker file,
`$TMPDIR/lo-e8-<name>`. It uses no network, deletes nothing and reads nothing. Lo passes while no
marker appears.

| File | What it tries | Marker |
|---|---|---|
| `.cargo/config.toml` | `rustc-wrapper = "e8/evil.sh"`, so cargo runs it in place of rustc in `e8/rust/` and `e8/pinned/` | `lo-e8-rustc-wrapper` |
| `.zed/settings.json` | points rust-analyzer, gopls, vtsls and basedpyright at `e8/evil.sh` | `lo-e8-rust-analyzer`, `lo-e8-gopls`, `lo-e8-vtsls`, `lo-e8-basedpyright` |
| `node_modules/typescript/lib/tsserver.js` | a workspace TypeScript whose server runs `e8/evil.sh` | `lo-e8-tsserver` |
| `.venv/`, `pyrightconfig.json` | a virtual environment at the root, where Zed finds it and hands it to basedpyright, whose `bin/python` runs `e8/evil.sh` | `lo-e8-python` |
| `e8/pinned/rust-toolchain.toml` | toolchain 1.999.0, which doesn't exist, so rustup tries to download it from `static.rust-lang.org` | none |
| `go.mod` | `toolchain go1.99`, which doesn't exist, so go tries to download it from `proxy.golang.org` | none |

The two toolchain files can't run anything here: rustup refuses a toolchain `path` relative to its
file, and go only runs a toolchain it downloads or finds on `PATH`. A check sees them in the
servers' environments instead (`ps -E -p <pid>`): rust-analyzer's has `RUSTUP_TOOLCHAIN` set and
`RUSTUP_AUTO_INSTALL=0`, and gopls's has `GOTOOLCHAIN=local` and `GOPROXY=off`. No connection goes
to `static.rust-lang.org` or `proxy.golang.org`. The toolchain file has its own crate, so it can't
stop cargo in `e8/rust/` before cargo reaches the wrapper.
EOF

  put "$root/e8/evil.sh" <<'EOF'
#!/bin/sh
# Fixture E8 for Lo's trust handling. If this runs, a file in the pull request ran it. It creates
# one empty marker file, $TMPDIR/lo-e8-<name>, where <name> says which file ran it, and does
# nothing else: no network, no deletes, no reads. set -C refuses a file or link that's already
# there, so it never writes through a planted link; a marker that's there already is enough.
set -C
case ${1-} in
  rust-analyzer | gopls | vtsls | basedpyright | tsserver | python) name=$1 ;;
  *) name=rustc-wrapper ;; # cargo runs a rustc wrapper with rustc's path first
esac
{ true >"${TMPDIR:-/tmp}/lo-e8-$name"; } 2>/dev/null || true
EOF
  chmod 755 "$root/e8/evil.sh"

  put "$root/.cargo/config.toml" <<'EOF'
# Fixture E8 for Lo's trust handling: cargo would run e8/evil.sh in place of rustc. It only creates
# the empty marker file $TMPDIR/lo-e8-rustc-wrapper.
[build]
rustc-wrapper = "e8/evil.sh"
EOF

  put "$root/.zed/settings.json" <<'EOF'
// Fixture E8 for Lo's trust handling: points each language server at e8/evil.sh, which only
// creates the empty marker file $TMPDIR/lo-e8-<server>.
{
  "lsp": {
    "rust-analyzer": { "binary": { "path": "e8/evil.sh", "arguments": ["rust-analyzer"] } },
    "gopls": { "binary": { "path": "e8/evil.sh", "arguments": ["gopls"] } },
    "vtsls": { "binary": { "path": "e8/evil.sh", "arguments": ["vtsls"] } },
    "basedpyright": { "binary": { "path": "e8/evil.sh", "arguments": ["basedpyright"] } }
  }
}
EOF

  put "$root/node_modules/typescript/package.json" <<'EOF'
{
  "name": "typescript",
  "version": "5.9.3",
  "description": "Fixture E8 for Lo's trust handling: a workspace TypeScript whose server runs e8/evil.sh."
}
EOF

  put "$root/node_modules/typescript/lib/tsserver.js" <<'EOF'
// Fixture E8 for Lo's trust handling: a workspace TypeScript server. It only runs e8/evil.sh,
// which creates the empty marker file $TMPDIR/lo-e8-tsserver.
const path = require("path");
require("child_process").execFileSync(path.join(__dirname, "..", "..", "..", "e8", "evil.sh"), ["tsserver"]);
EOF

  # Zed's environment finder, pet, looks for a .venv in the worktree's root, and takes a folder with
  # a pyvenv.cfg for a virtual environment. Zed hands the one it picks to basedpyright as its
  # interpreter.
  put "$root/pyrightconfig.json" <<'EOF'
// Fixture E8 for Lo's trust handling: the virtual environment .venv, whose bin/python only runs
// e8/evil.sh, which creates the empty marker file $TMPDIR/lo-e8-python.
{
  "venvPath": ".",
  "venv": ".venv"
}
EOF

  put "$root/.venv/pyvenv.cfg" <<'EOF'
home = /usr/bin
include-system-site-packages = false
version = 3.12.0
EOF

  put "$root/.venv/bin/python" <<'EOF'
#!/bin/sh
# Fixture E8 for Lo's trust handling: an interpreter that only runs e8/evil.sh.
exec "$(dirname "$0")/../../e8/evil.sh" python
EOF
  chmod 755 "$root/.venv/bin/python"

  put "$root/go.mod" <<'EOF'
// Fixture E8 for Lo's trust handling: a Go toolchain that doesn't exist, so go would try to
// download it from proxy.golang.org. Nothing here can run; e8/README.md says how a check sees it.
module example.com/e8

go 1.21

toolchain go1.99
EOF

  put "$root/e8/rust/Cargo.toml" <<'EOF'
[package]
name = "e8"
version = "0.1.0"
edition = "2021"

[dependencies]
EOF

  put "$root/e8/rust/src/lib.rs" <<'EOF'
//! Fixture E8: a Rust crate, so rust-analyzer has something to open.

pub fn answer() -> u32 {
    42
}
EOF

  put "$root/e8/pinned/Cargo.toml" <<'EOF'
# Fixture E8: a crate of its own for the toolchain file, so it can't stop cargo in e8/rust/ before
# cargo reaches the rustc wrapper.
[package]
name = "e8-pinned"
version = "0.1.0"
edition = "2021"

[dependencies]
EOF

  put "$root/e8/pinned/rust-toolchain.toml" <<'EOF'
# Fixture E8 for Lo's trust handling: a toolchain that doesn't exist, so rustup would try to
# download it from static.rust-lang.org. Nothing here can run; e8/README.md says how a check sees it.
[toolchain]
channel = "1.999.0"
EOF

  put "$root/e8/pinned/src/lib.rs" <<'EOF'
//! Fixture E8: a crate whose rust-toolchain.toml names a toolchain that doesn't exist.

pub fn answer() -> u32 {
    42
}
EOF

  put "$root/e8/ts/index.ts" <<'EOF'
// Fixture E8: TypeScript, so a TypeScript server has something to open.

export function answer(): number {
  return 42;
}
EOF

  put "$root/e8/py/app.py" <<'EOF'
"""Fixture E8: Python, so a Python server has something to open."""


def answer():
    return 42
EOF

  put "$root/e8/go/main.go" <<'EOF'
// Fixture E8: Go, so gopls has something to open.
package main

import "fmt"

func main() {
	fmt.Println(42)
}
EOF
}

# --- E9: moves and syntax -----------------------------------------------------------------------

# Moves, syntax-aware cases and fallbacks:
# - fullName moves from api/user.ts into the Names class in api/names.ts: re-indented, with its
#   return line edited. user.ts also loses legacyName outright, for a thread on a line only the
#   old side has;
# - checksum moves up lib.rs, unchanged;
# - two lines move down short.py, too few to count as a move;
# - syntax/rename.rs renames a variable, syntax/args.ts reorders arguments, syntax/wrap.py wraps
#   a block in a new if, and syntax/config.json reorders keys;
# - broken.ts gains a parse error, and large.ts is 1.2 MB on both sides.
e9_tree() {
  local side=$1 dir=${2:?}/e9
  check_side "$side" || return 1
  rm -rf "$dir"

  if [[ $side == base ]]; then
    put "$dir/api/user.ts" <<'EOF'
// Fixture E9: fullName moves from here into the Names class in names.ts.

export interface User {
  first: string;
  middle?: string;
  last: string;
  email: string;
}

export function emailDomain(user: User): string {
  return user.email.split("@")[1] ?? "";
}

export function fullName(user: User): string {
  const first = user.first.trim();
  const middle = user.middle?.trim();
  const last = user.last.trim();
  return [first, middle, last].filter(Boolean).join(" ");
}

export function isValid(user: User): boolean {
  return user.first.length > 0 && user.last.length > 0;
}

export function legacyName(user: User): string {
  return `${user.last.toUpperCase()}, ${user.first}`;
}
EOF
    put "$dir/api/names.ts" <<'EOF'
// Fixture E9: fullName moves here from user.ts.

import type { User } from "./user";

export class Names {
  constructor(private readonly users: User[]) {}

  sorted(): string[] {
    return this.users.map((user) => user.last).sort();
  }
}
EOF
  else
    put "$dir/api/user.ts" <<'EOF'
// Fixture E9: fullName moves from here into the Names class in names.ts.

export interface User {
  first: string;
  middle?: string;
  last: string;
  email: string;
}

export function emailDomain(user: User): string {
  return user.email.split("@")[1] ?? "";
}

export function isValid(user: User): boolean {
  return user.first.length > 0 && user.last.length > 0;
}
EOF
    put "$dir/api/names.ts" <<'EOF'
// Fixture E9: fullName moves here from user.ts.

import type { User } from "./user";

export class Names {
  constructor(private readonly users: User[]) {}

  fullName(user: User): string {
    const first = user.first.trim();
    const middle = user.middle?.trim();
    const last = user.last.trim();
    return [first, middle, last].filter((name) => name).join(" ");
  }

  sorted(): string[] {
    return this.users.map((user) => user.last).sort();
  }
}
EOF
  fi

  # The lines that stay put outnumber checksum's, so a line diff moves checksum, not them.
  if [[ $side == base ]]; then
    put "$dir/lib.rs" <<'EOF'
//! Fixture E9: `checksum` moves up this file, unchanged.

pub fn parse_header(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once(':')?;
    Some((name.trim(), value.trim()))
}

pub fn parse_headers(text: &str) -> Vec<(&str, &str)> {
    text.lines().filter_map(parse_header).collect()
}

pub fn header_names(text: &str) -> Vec<&str> {
    parse_headers(text).into_iter().map(|(name, _)| name).collect()
}

pub fn has_header(text: &str, wanted: &str) -> bool {
    header_names(text)
        .iter()
        .any(|name| name.eq_ignore_ascii_case(wanted))
}

pub fn checksum(bytes: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for byte in bytes {
        sum = sum.wrapping_mul(31).wrapping_add(u32::from(*byte));
    }
    sum
}
EOF
  else
    put "$dir/lib.rs" <<'EOF'
//! Fixture E9: `checksum` moves up this file, unchanged.

pub fn parse_header(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once(':')?;
    Some((name.trim(), value.trim()))
}

pub fn checksum(bytes: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for byte in bytes {
        sum = sum.wrapping_mul(31).wrapping_add(u32::from(*byte));
    }
    sum
}

pub fn parse_headers(text: &str) -> Vec<(&str, &str)> {
    text.lines().filter_map(parse_header).collect()
}

pub fn header_names(text: &str) -> Vec<&str> {
    parse_headers(text).into_iter().map(|(name, _)| name).collect()
}

pub fn has_header(text: &str, wanted: &str) -> bool {
    header_names(text)
        .iter()
        .any(|name| name.eq_ignore_ascii_case(wanted))
}
EOF
  fi

  # describe() is longer than the two lines, so a line diff moves the two lines, not it.
  if [[ $side == base ]]; then
    put "$dir/short.py" <<'EOF'
"""Fixture E9: two lines move below describe(), too few to count as a move."""

TIMEOUT_SECONDS = 30
RETRY_LIMIT = 3


def describe():
    parts = [
        f"timeout {TIMEOUT_SECONDS}s",
        f"{RETRY_LIMIT} retries",
    ]
    return ", ".join(parts)


def main():
    print(describe())
EOF
  else
    put "$dir/short.py" <<'EOF'
"""Fixture E9: two lines move below describe(), too few to count as a move."""


def describe():
    parts = [
        f"timeout {TIMEOUT_SECONDS}s",
        f"{RETRY_LIMIT} retries",
    ]
    return ", ".join(parts)


TIMEOUT_SECONDS = 30
RETRY_LIMIT = 3


def main():
    print(describe())
EOF
  fi

  if [[ $side == base ]]; then
    put "$dir/syntax/rename.rs" <<'EOF'
//! Fixture E9: `total` is renamed to `sum`, so only the name changes.

pub fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let total: f64 = values.iter().sum();
    total / values.len() as f64
}
EOF
    put "$dir/syntax/args.ts" <<'EOF'
// Fixture E9: clamp's arguments are reordered, in its signature and at its call.

export function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

export function percent(done: number, total: number): number {
  return clamp((done / total) * 100, 0, 100);
}
EOF
    put "$dir/syntax/wrap.py" <<'EOF'
"""Fixture E9: a block is wrapped in a new if, so only the if is new."""


def save(path, data, log):
    tmp = path + ".tmp"
    with open(tmp, "wb") as out:
        out.write(data)
    log.append(f"saved {len(data)} bytes to {path}")
    return tmp
EOF
    put "$dir/syntax/config.json" <<'EOF'
{
  "name": "e9",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "build": "tsc",
    "test": "node --test"
  },
  "license": "MIT"
}
EOF
    put "$dir/broken.ts" <<'EOF'
// Fixture E9: the new version has a parse error, so the syntax-aware view falls back.

export function total(items: number[]): number {
  let sum = 0;
  for (const item of items) {
    sum += item;
  }
  return sum;
}
EOF
  else
    put "$dir/syntax/rename.rs" <<'EOF'
//! Fixture E9: `total` is renamed to `sum`, so only the name changes.

pub fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let sum: f64 = values.iter().sum();
    sum / values.len() as f64
}
EOF
    put "$dir/syntax/args.ts" <<'EOF'
// Fixture E9: clamp's arguments are reordered, in its signature and at its call.

export function clamp(min: number, max: number, value: number): number {
  return Math.min(Math.max(value, min), max);
}

export function percent(done: number, total: number): number {
  return clamp(0, 100, (done / total) * 100);
}
EOF
    put "$dir/syntax/wrap.py" <<'EOF'
"""Fixture E9: a block is wrapped in a new if, so only the if is new."""


def save(path, data, log):
    tmp = path + ".tmp"
    if data:
        with open(tmp, "wb") as out:
            out.write(data)
        log.append(f"saved {len(data)} bytes to {path}")
    return tmp
EOF
    put "$dir/syntax/config.json" <<'EOF'
{
  "version": "1.0.0",
  "name": "e9",
  "private": true,
  "license": "MIT",
  "scripts": {
    "test": "node --test",
    "build": "tsc"
  }
}
EOF
    put "$dir/broken.ts" <<'EOF'
// Fixture E9: the new version has a parse error, so the syntax-aware view falls back.

export function total(items: number[]): number {
  let sum = 0;
  for (const item of items {
    sum += item * 2;
  }
  return sum;
}
EOF
  fi

  # 20,000 rows of about 62 bytes; the head side changes three of them.
  awk -v side="$side" 'BEGIN {
    state = 9
    print "// Fixture E9: over 1 MB, so the syntax-aware view falls back to the line diff."
    print "export const ROWS: Array<[number, string]> = ["
    for (i = 1; i <= 20000; i++) {
      words = ""
      for (w = 1; w <= 4; w++) {
        state = (state * 16807) % 2147483647
        words = words sprintf(" %08x", state)
      }
      if (side == "head" && (i == 5000 || i == 12000 || i == 18000)) words = words " changed"
      printf "  [%d, \"row %06d:%s\"],\n", i, i, words
    }
    print "];"
  }' >"$dir/large.ts"
}

# --- E10: large ---------------------------------------------------------------------------------

# 300 files and about 25,000 changed lines:
# - 298 files of TypeScript, TypeScript tests, Rust, Python, Go and TOML. The head side changes
#   253, adds 29 and deletes 16, and moves functions within files and between them;
# - e10/src/big/catalog.ts, 6,000 lines, which the head side adds;
# - e10/package-lock.json, a lockfile the head side changes.
e10_tree() {
  local side=$1 dir=${2:?}/e10
  check_side "$side" || return 1
  rm -rf "$dir"
  mkdir -p "$dir"/{src,rust,py,go,config}/m{0,1,2,3,4,5,6,7,8,9,10,11} "$dir/src/big"
  awk -v side="$side" -v root="$2" '
    function next_random() { state = (state * 16807) % 2147483647; return state }
    function below(n) { return next_random() % n }
    function reseed(n) { state = n % 2147483646 + 1; next_random(); next_random() }

    function out(line) { print line > file }

    # Kinds: 0 TypeScript, 1 Rust, 2 Python, 3 Go, 4 TypeScript tests, 5 TOML.
    function path_of(i,   k, m) {
      k = i % 6
      m = "m" (i % 12)
      if (k == 0) return "e10/src/" m "/f" i ".ts"
      if (k == 1) return "e10/rust/" m "/f" i ".rs"
      if (k == 2) return "e10/py/" m "/f" i ".py"
      if (k == 3) return "e10/go/" m "/f" i ".go"
      if (k == 4) return "e10/src/" m "/f" i ".test.ts"
      return "e10/config/" m "/f" i ".toml"
    }

    function status_of(i) {
      if (i % 10 == 0) return "added"
      if (i % 17 == 0) return "deleted"
      return "modified"
    }

    function prelude(k, i) {
      if (k == 0) out("// Fixture E10, file " i ".")
      else if (k == 1) out("//! Fixture E10, file " i ".")
      else if (k == 2) out("\"\"\"Fixture E10, file " i ".\"\"\"")
      else if (k == 3) { out("// Fixture E10, file " i "."); out("package m" (i % 12)) }
      else if (k == 4) { out("// Fixture E10, file " i "."); out("import { compute } from \"./compute\";") }
      else out("# Fixture E10, file " i ".")
      out("")
    }

    function header(k, name) {
      if (k == 0) return "export function " name "(x: number): number {"
      if (k == 1) return "pub fn " name "(x: i64) -> i64 {"
      if (k == 2) return "def " name "(x):"
      if (k == 3) return "func " toupper(substr(name, 1, 1)) substr(name, 2) "(x int) int {"
      if (k == 4) return "test(\"" name "\", () => {"
      return "[" name "]"
    }

    # <c> numbers TOML keys, so a section never repeats one.
    function body(k, c, a, b) {
      if (k == 0) return "  x = x * " a " + " b ";"
      if (k == 1) return "    let x = x * " a " + " b ";"
      if (k == 2) return "    x = x * " a " + " b
      if (k == 3) return "\tx = x*" a " + " b
      if (k == 4) return "  expect(compute(" a ")).toBe(" b ");"
      return "key_" c " = " (a * 1000 + b)
    }

    function footer(k) {
      if (k == 0) { out("  return x;"); out("}") }
      else if (k == 1) { out("    x"); out("}") }
      else if (k == 2) out("    return x")
      else if (k == 3) { out("\treturn x"); out("}") }
      else if (k == 4) out("});")
      out("")
    }

    # File <i> as the base side has it: F["n"] functions, each with a name, a length and lines.
    function generate(i, F,   k, n, j, len, l, c) {
      delete F
      reseed(i * 7919)
      k = i % 6
      F["n"] = n = 4 + below(6)
      c = 0
      for (j = 1; j <= n; j++) {
        F[j, "name"] = "f" i "_" j
        F[j, "len"] = len = 3 + below(12)
        for (l = 1; l <= len; l++) F[j, l] = body(k, ++c, 2 + below(97), below(1000))
      }
    }

    function emit(k, F, j,   l) {
      out(header(k, F[j, "name"]))
      for (l = 1; l <= F[j, "len"]; l++) out(F[j, l])
      footer(k)
    }

    function new_line(k) { return body(k, ++fresh, 2 + below(97), below(1000)) }

    # Function <j> with one stretch of its lines replaced, and sometimes a second.
    function emit_edited(k, F, j,   len, l, start, cut, add, n, edits, e) {
      len = F[j, "len"]
      out(header(k, F[j, "name"]))
      edits = 1 + (below(3) == 0)
      l = 1
      for (e = 1; e <= edits && l <= len; e++) {
        start = l + below(len - l + 1)
        cut = below(4)
        if (start + cut - 1 > len) cut = len - start + 1
        add = 1 + below(7)
        for (; l < start; l++) out(F[j, l])
        for (n = 1; n <= add; n++) out(new_line(k))
        l = start + cut
      }
      for (; l <= len; l++) out(F[j, l])
      footer(k)
    }

    function emit_new(k, name,   len, n) {
      out(header(k, name))
      len = 4 + below(10)
      for (n = 1; n <= len; n++) out(new_line(k))
      footer(k)
    }

    # Whether file <i> moves its second function to file <i> + 6, the next of its kind.
    function moves_out(i) {
      return i % 25 == 7 && i + 6 <= FILES && status_of(i) == "modified" && status_of(i + 6) == "modified"
    }

    # The head side of a changed file. Each function is kept, edited or deleted, and may have a new
    # one after it. Every ninth file moves its first function to its end.
    function write_head(i, k,   n, j, r, changed, out_move, in_move, inner, saved) {
      n = F["n"]
      reseed(i * 104729 + 17)
      fresh = 1000
      out_move = moves_out(i)
      in_move = i > 6 && moves_out(i - 6)
      inner = i % 9 == 4
      changed = out_move || in_move || inner
      for (j = 1; j <= n; j++) {
        r = below(100)
        fate[j] = r < 12 ? "delete" : r < 72 ? "edit" : "keep"
        if ((out_move && j == 2) || (inner && j == 1)) fate[j] = "keep"
        extra[j] = below(100) < 28
        if (fate[j] != "keep" || extra[j]) changed = 1
      }
      if (!changed) fate[1] = "edit"
      for (j = 1; j <= n; j++) {
        if (in_move && j == 2) {
          saved = state
          generate(i - 6, G)
          state = saved
          emit(k, G, 2)
        }
        if (!(out_move && j == 2) && !(inner && j == 1)) {
          if (fate[j] == "edit") emit_edited(k, F, j)
          else if (fate[j] == "keep") emit(k, F, j)
        }
        if (extra[j]) emit_new(k, "g" i "_" j)
      }
      if (inner) emit(k, F, 1)
    }

    function write_file(i,   status, k, j) {
      status = status_of(i)
      if (side == "base" && status == "added") return
      if (side == "head" && status == "deleted") return
      file = root "/" path_of(i)
      k = i % 6
      generate(i, F)
      prelude(k, i)
      if (side == "head" && status == "modified") write_head(i, k)
      else for (j = 1; j <= F["n"]; j++) emit(k, F, j)
      close(file)
    }

    function integrity(p, major,   s, n) {
      reseed(p * 7 + major * 100003)
      s = ""
      for (n = 1; n <= 86; n++) s = s substr(ALPHABET, 1 + below(64), 1)
      return "sha512-" s "=="
    }

    # An npm lockfile of 450 packages. The head side bumps every fourth package a major version,
    # drops every 31st and adds 40.
    function write_lockfile(   total, p, last, major, version, name) {
      file = root "/e10/package-lock.json"
      total = side == "head" ? 490 : 450
      for (p = 1; p <= total; p++) if (!(side == "head" && p <= 450 && p % 31 == 0)) last = p
      out("{")
      out("  \"name\": \"e10\",")
      out("  \"version\": \"1.0.0\",")
      out("  \"lockfileVersion\": 3,")
      out("  \"requires\": true,")
      out("  \"packages\": {")
      out("    \"\": {")
      out("      \"name\": \"e10\",")
      out("      \"version\": \"1.0.0\"")
      out("    },")
      for (p = 1; p <= total; p++) {
        if (side == "head" && p <= 450 && p % 31 == 0) continue
        major = p > 450 ? 0 : side == "head" && p % 4 == 1 ? 2 : 1
        version = major "." (p % 9) "." (p % 13)
        name = sprintf("pkg-%04d", p)
        out("    \"node_modules/@e10/" name "\": {")
        out("      \"version\": \"" version "\",")
        out("      \"resolved\": \"https://registry.npmjs.org/@e10/" name "/-/" name "-" version ".tgz\",")
        out("      \"integrity\": \"" integrity(p, major) "\",")
        out("      \"license\": \"MIT\"")
        out("    }" (p == last ? "" : ","))
      }
      out("  }")
      out("}")
      close(file)
    }

    # 6,000 lines, which only the head side has.
    function write_catalog(   n) {
      file = root "/e10/src/big/catalog.ts"
      reseed(6000)
      out("// Fixture E10: a 6,000-line file.")
      out("export const CATALOG = [")
      for (n = 1; n <= 5997; n++)
        out(sprintf("  { id: %d, sku: \"E10-%06d\", price: %d, stock: %d },", n, n, 100 + below(99900), below(500)))
      out("];")
      close(file)
    }

    BEGIN {
      FILES = 298
      ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
      for (i = 1; i <= FILES; i++) write_file(i)
      write_lockfile()
      if (side == "head") write_catalog()
    }'
}

# --- E11: review writes ------------------------------------------------------------------------

# A 60-line text file whose head changes lines 10, 11, 30 and 50, so the diff has three hunks
# (lines 7–14, 27–33 and 47–53) with lines outside them, and a short Markdown file with one line
# changed, for file-level comments. Lo's recordings of comments, replies and reviews are written
# here (crates/github/tests/record_fixtures.rs).
e11_tree() {
  local side=$1 dir=${2:?}/e11 n
  check_side "$side" || return 1
  rm -rf "$dir"
  mkdir -p "$dir"
  for ((n = 1; n <= 60; n++)); do
    case $side:$n in
      head:10 | head:11 | head:30 | head:50) echo "line $n changed" ;;
      *) echo "line $n" ;;
    esac
  done >"$dir/review.txt"
  put "$dir/notes.md" <<EOF
# Review writes

Fixture E11: Lo comments, replies and reviews here as rozed-bot.

This line is the $side side's.
EOF
}

# --- E12: languages from bundled extensions ----------------------------------------------------

# One file each in Swift, Kotlin, Terraform, Ruby, Java, C#, PHP, Elixir, TOML, HTML, a Dockerfile
# and a Makefile, each changed by the head side. None is part of a project a language server would
# build: no Package.swift, Gradle, mix.exs or the like.
e12_tree() {
  local side=$1 dir=${2:?}/e12
  check_side "$side" || return 1
  rm -rf "$dir"

  if [[ $side == base ]]; then
    put "$dir/swift/Greeter.swift" <<'EOF'
// Fixture E12: Swift.
struct Greeter {
    let name: String

    func greet() -> String {
        "Hello, \(name)!"
    }
}
EOF
    put "$dir/kotlin/Greeter.kt" <<'EOF'
// Fixture E12: Kotlin.
class Greeter(private val name: String) {
    fun greet(): String = "Hello, $name!"
}
EOF
    put "$dir/terraform/main.tf" <<'EOF'
# Fixture E12: Terraform, with no providers or resources.
variable "name" {
  type    = string
  default = "world"
}

output "greeting" {
  value = "Hello, ${var.name}!"
}
EOF
    put "$dir/ruby/greeter.rb" <<'EOF'
# Fixture E12: Ruby.
class Greeter
  def initialize(name)
    @name = name
  end

  def greet
    "Hello, #{@name}!"
  end
end
EOF
    put "$dir/java/Greeter.java" <<'EOF'
// Fixture E12: Java.
public final class Greeter {
    private final String name;

    public Greeter(String name) {
        this.name = name;
    }

    public String greet() {
        return "Hello, " + name + "!";
    }
}
EOF
    put "$dir/csharp/Greeter.cs" <<'EOF'
// Fixture E12: C#.
namespace E12;

public sealed class Greeter(string name)
{
    public string Greet() => $"Hello, {name}!";
}
EOF
    put "$dir/php/greeter.php" <<'EOF'
<?php
// Fixture E12: PHP.

final class Greeter
{
    public function __construct(private string $name)
    {
    }

    public function greet(): string
    {
        return "Hello, {$this->name}!";
    }
}
EOF
    put "$dir/elixir/greeter.ex" <<'EOF'
# Fixture E12: Elixir.
defmodule Greeter do
  def greet(name) do
    "Hello, #{name}!"
  end
end
EOF
    put "$dir/toml/settings.toml" <<'EOF'
# Fixture E12: TOML.
[greeting]
name = "world"
punctuation = "!"
EOF
    put "$dir/html/index.html" <<'EOF'
<!doctype html>
<!-- Fixture E12: HTML. -->
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Greeting</title>
  </head>
  <body>
    <h1>Hello, world!</h1>
  </body>
</html>
EOF
    put "$dir/docker/Dockerfile" <<'EOF'
# Fixture E12: a Dockerfile.
FROM alpine:3.20
WORKDIR /app
COPY greeting.txt .
CMD ["cat", "greeting.txt"]
EOF
    mkdir -p "$dir/make"
    printf '%s\n' '# Fixture E12: a Makefile.' 'NAME ?= world' '' 'greet:' \
      $'\t@echo "Hello, $(NAME)!"' '' '.PHONY: greet' >"$dir/make/Makefile"
  else
    put "$dir/swift/Greeter.swift" <<'EOF'
// Fixture E12: Swift.
struct Greeter {
    let name: String
    var excited = false

    func greet() -> String {
        excited ? "Hello, \(name)!!" : "Hello, \(name)!"
    }
}
EOF
    put "$dir/kotlin/Greeter.kt" <<'EOF'
// Fixture E12: Kotlin.
class Greeter(private val name: String, private val excited: Boolean = false) {
    fun greet(): String = if (excited) "Hello, $name!!" else "Hello, $name!"
}
EOF
    put "$dir/terraform/main.tf" <<'EOF'
# Fixture E12: Terraform, with no providers or resources.
variable "name" {
  type    = string
  default = "world"
}

variable "excited" {
  type    = bool
  default = false
}

output "greeting" {
  value = var.excited ? "Hello, ${var.name}!!" : "Hello, ${var.name}!"
}
EOF
    put "$dir/ruby/greeter.rb" <<'EOF'
# Fixture E12: Ruby.
class Greeter
  def initialize(name, excited: false)
    @name = name
    @excited = excited
  end

  def greet
    @excited ? "Hello, #{@name}!!" : "Hello, #{@name}!"
  end
end
EOF
    put "$dir/java/Greeter.java" <<'EOF'
// Fixture E12: Java.
public final class Greeter {
    private final String name;
    private final boolean excited;

    public Greeter(String name, boolean excited) {
        this.name = name;
        this.excited = excited;
    }

    public String greet() {
        return "Hello, " + name + (excited ? "!!" : "!");
    }
}
EOF
    put "$dir/csharp/Greeter.cs" <<'EOF'
// Fixture E12: C#.
namespace E12;

public sealed class Greeter(string name, bool excited = false)
{
    public string Greet() => excited ? $"Hello, {name}!!" : $"Hello, {name}!";
}
EOF
    put "$dir/php/greeter.php" <<'EOF'
<?php
// Fixture E12: PHP.

final class Greeter
{
    public function __construct(private string $name, private bool $excited = false)
    {
    }

    public function greet(): string
    {
        return "Hello, {$this->name}" . ($this->excited ? "!!" : "!");
    }
}
EOF
    put "$dir/elixir/greeter.ex" <<'EOF'
# Fixture E12: Elixir.
defmodule Greeter do
  def greet(name, excited \\ false) do
    if excited, do: "Hello, #{name}!!", else: "Hello, #{name}!"
  end
end
EOF
    put "$dir/toml/settings.toml" <<'EOF'
# Fixture E12: TOML.
[greeting]
name = "world"
punctuation = "!!"
excited = true
EOF
    put "$dir/html/index.html" <<'EOF'
<!doctype html>
<!-- Fixture E12: HTML. -->
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Greeting</title>
  </head>
  <body>
    <h1>Hello, world!!</h1>
    <p>Excited, too.</p>
  </body>
</html>
EOF
    put "$dir/docker/Dockerfile" <<'EOF'
# Fixture E12: a Dockerfile.
FROM alpine:3.21
RUN adduser -D app
USER app
WORKDIR /app
COPY greeting.txt .
CMD ["cat", "greeting.txt"]
EOF
    mkdir -p "$dir/make"
    printf '%s\n' '# Fixture E12: a Makefile.' 'NAME ?= world' '' 'greet:' \
      $'\t@echo "Hello, $(NAME)!"' '' 'shout:' $'\t@echo "HELLO, $(NAME)!!"' '' \
      '.PHONY: greet shout' >"$dir/make/Makefile"
  fi
}

# --- #17: images --------------------------------------------------------------------------------

# Two of the SVGs' comments use the project's first name, as on GitHub.
readonly FIRST_NAME=rozed # rename:keep

# rozed-dev/e2e#17's files, byte for byte: SVGs whose <image>s point at files on the reviewer's
# disk and at /dev/zero, a 316-byte pattern bomb with a 20000 x 20000 tile, and a 48 x 48 PNG.
images_tree() {
  local dir=${1:?}/e5
  rm -rf "$dir"
  mkdir -p "$dir"

  cat >"$dir/desktop-picture.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="400" height="200" viewBox="0 0 400 200">
  <!-- Fixture: only the frame and the orange dot should draw. The three images point at a
       macOS desktop picture (solid teal); if any teal shows, $FIRST_NAME read a file from disk. -->
  <rect x="4" y="4" width="392" height="192" fill="#f4f4f4" stroke="#333" stroke-width="8"/>
  <circle cx="40" cy="40" r="20" fill="#e07000"/>
  <image x="80" y="20" width="90" height="160" href="/System/Library/Desktop Pictures/Solid Colors/Teal.png"/>
  <image x="180" y="20" width="90" height="160" xlink:href="/System/Library/Desktop Pictures/Solid Colors/Teal.png"/>
  <image x="280" y="20" width="90" height="160" href="file:///System/Library/Desktop%20Pictures/Solid%20Colors/Teal.png"/>
</svg>
EOF

  cat >"$dir/dev-zero.svg" <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="400" height="200" viewBox="0 0 400 200">
  <!-- Fixture: only the frame and the purple dot should draw, quickly. The images point at
       /dev/zero, which never ends; reading it would hang or exhaust memory. -->
  <rect x="4" y="4" width="392" height="192" fill="#f4f4f4" stroke="#333" stroke-width="8"/>
  <circle cx="40" cy="40" r="20" fill="#8030c0"/>
  <image x="80" y="20" width="140" height="160" href="/dev/zero"/>
  <image x="240" y="20" width="140" height="160" xlink:href="file:///dev/zero"/>
</svg>
EOF

  cat >"$dir/local-image.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="400" height="200" viewBox="0 0 400 200">
  <!-- Fixture: only the frame and the blue dot should draw. The images point at macOS's default
       wallpaper photo (through a symlink); if a photo shows, $FIRST_NAME read a file from disk. -->
  <rect x="4" y="4" width="392" height="192" fill="#f4f4f4" stroke="#333" stroke-width="8"/>
  <circle cx="40" cy="40" r="20" fill="#1060e0"/>
  <image x="80" y="20" width="140" height="160" preserveAspectRatio="xMidYMid slice" href="/System/Library/CoreServices/DefaultBackground.jpg"/>
  <image x="240" y="20" width="140" height="160" preserveAspectRatio="xMidYMid slice" xlink:href="../../../../../../System/Library/CoreServices/DefaultBackground.jpg"/>
</svg>
EOF

  # No newline at the end, as on GitHub.
  printf '%s' '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200"><defs><pattern id="p" width="20000" height="20000" patternUnits="userSpaceOnUse"><rect width="20000" height="20000" fill="#c00"/><circle cx="10000" cy="10000" r="9000" fill="#0c0"/></pattern></defs><rect width="200" height="200" fill="url(#p)"/></svg>' \
    >"$dir/pattern-bomb.svg"

  base64 --decode >"$dir/small.png" <<'EOF'
iVBORw0KGgoAAAANSUhEUgAAADAAAAAwCAIAAADYYG7QAAAQJ0lEQVR42hXYobpFUBCG4V0URVEURVEURVEURVEURVEURVEURVEU
RfmKoiiKoiiKoiiKoriJ47iC97FmzT+zfr8fwg/xh/RD/qH8UH9oP/Qfxg/zh/XD/uH8cH94P/wfwY/wR/Qj/pH8SH9kP/IfxY/y
R/Wj/tH8aH90P/jR/xh+jD+mH/OP5cf6Y/ux/zh+nD+uH/eP58f74/cTEAREAUlAFlAEVAFNQBcwBEwBS8AWcARcAU/AFwgEQoFI
IBZIBFKBTCAXKARKgUqgFmgEWoFOAIFeYBAYBSaBWWARWAU2gV3gEDgFLoFb4BF4hQ8kIoiIIpKILKKIqCKaiC5iiJgilogt4oi4
Ip6ILxKIhCKRSCySiKQimUguUoiUIpVILdKItCKdCCK9yCAyikwis8gisopsIrvIIXKKXCK3yCPyih9IQpAQJSQJWUKRUCU0CV3C
kDAlLAlbwpFwJTwJXyKQCCUiiVgikUglMolcopAoJSqJWqKRaCU6CSR6iUFilJgkZolFYpXYJHaJQ+KUuCRuiUfilT6QjCAjykgy
sowio8poMrqMIWPKWDK2jCPjyngyvkwgE8pEMrFMIpPKZDK5TCFTylQytUwj08p0Msj0MoPMKDPJzDKLzCqzyewyh8wpc8ncMo/M
K38gBUFBVJAUZAVFQVXQFHQFQ8FUsBRsBUfBVfAUfIVAIVSIFGKFRCFVyBRyhUKhVKgUaoVGoVXoFFDoFQaFUWFSmBUWhVVhU9gV
DoVT4VK4FR6FV/lAKoKKqCKpyCqKiqqiqegqhoqpYqnYKo6Kq+Kp+CqBSqgSqcQqiUqqkqnkKoVKqVKp1CqNSqvSqaDSqwwqo8qk
MqssKqvKprKrHCqnyqVyqzwqr/qBNAQNUUPSkDUUDVVD09A1DA1Tw9KwNRwNV8PT8DUCjVAj0og1Eo1UI9PINQqNUqPSqDUajVaj
00Cj1xg0Ro1JY9ZYNFaNTWPXODROjUvj1ng0Xu0D6Qg6oo6kI+soOqqOpqPrGDqmjqVj6zg6ro6n4+sEOqFOpBPrJDqpTqaT6xQ6
pU6lU+s0Oq1Op4NOrzPojDqTzqyz6Kw6m86uc+icOpfOrfPovPoHMhAMRAPJQDZQDFQDzUA3MAxMA8vANnAMXAPPwDcIDEKDyCA2
SAxSg8wgNygMSoPKoDZoDFqDzgCD3mAwGA0mg9lgMVgNNoPd4DA4DS6D2+AxeI0PZCKYiCaSiWyimKgmmoluYpiYJpaJbeKYuCae
iW8SmIQmkUlskpikJplJblKYlCaVSW3SmLQmnQkmvclgMppMJrPJYrKabCa7yWFymlwmt8lj8pofyEKwEC0kC9lCsVAtNAvdwrAw
LSwL28KxcC08C98isAgtIovYIrFILTKL3KKwKC0qi9qisWgtOgsseovBYrSYLGaLxWK12Cx2i8PitLgsbovH4rU+kI1gI9pINrKN
YqPaaDa6jWFj2lg2to1j49p4Nr5NYBPaRDaxTWKT2mQ2uU1hU9pUNrVNY9PadDbY9DaDzWgz2cw2i81qs9nsNofNaXPZ3DaPzWt/
IAfBQXSQHGQHxUF10Bx0B8PBdLAcbAfHwXXwHHyHwCF0iBxih8QhdcgccofCoXSoHGqHxqF16Bxw6B0Gh9FhcpgdFofVYXPYHQ6H
0+FyuB0eh9f5QC6Ci+giucguiovqornoLoaL6WK52C6Oi+viufgugUvoErnELolL6pK55C6FS+lSudQujUvr0rng0rsMLqPL5DK7
LC6ry+ayuxwup8vlcrs8Lq/7gTwED9FD8pA9FA/VQ/PQPQwP08PysD0cD9fD8/A9Ao/QI/KIPRKP1CPzyD0Kj9Kj8qg9Go/Wo/PA
o/cYPEaPyWP2WDxWj81j9zg8To/L4/Z4PF7vA/kIPqKP5CP7KD6qj+aj+xg+po/lY/s4Pq6P5+P7BD6hT+QT+yQ+qU/mk/sUPqVP
5VP7ND6tT+eDT+8z+Iw+k8/ss/isPpvP7nP4nD6Xz+3z+Lz+BwoQAsQAKUAOUALUAC1ADzACzAArwA5wAtwAL8APCALCgCggDkgC
0oAsIA8oAsqAKqAOaALagC6AgD5gCBgDpoA5YAlYA7aAPeAIOAOugDvgCXiDDxQihIghUogcooSoIVqIHmKEmCFWiB3ihLghXogf
EoSEIVFIHJKEpCFZSB5ShJQhVUgd0oS0IV0IIX3IEDKGTCFzyBKyhmwhe8gRcoZcIXfIE/KGHyhCiBAjpAg5QolQI7QIPcKIMCOs
CDvCiXAjvAg/IogII6KIOCKJSCOyiDyiiCgjqog6ooloI7oIIvqIIWKMmCLmiCVijdgi9ogj4oy4Iu6IJ+KNPlCMECPGSDFyjBKj
xmgxeowRY8ZYMXaME+PGeDF+TBATxkQxcUwSk8ZkMXlMEVPGVDF1TBPTxnQxxPQxQ8wYM8XMMUvMGrPF7DFHzBlzxdwxT8wbf6AE
IUFMkBLkBCVBTdAS9AQjwUywEuwEJ8FN8BL8hCAhTIgS4oQkIU3IEvKEIqFMqBLqhCahTegSSOgThoQxYUqYE5aENWFL2BOOhDPh
SrgTnoQ3+UApQoqYIqXIKUqKmqKl6ClGiplipdgpToqb4qX4KUFKmBKlxClJSpqSpeQpRUqZUqXUKU1Km9KlkNKnDCljypQypywp
a8qWsqccKWfKlXKnPClv+oEyhAwxQ8qQM5QMNUPL0DOMDDPDyrAznAw3w8vwM4KMMCPKiDOSjDQjy8gziowyo8qoM5qMNqPLIKPP
GDLGjCljzlgy1owtY884Ms6MK+POeDLe7APlCDlijpQj5yg5ao6Wo+cYOWaOlWPnODlujpfj5wQ5YU6UE+ckOWlOlpPnFDllTpVT
5zQ5bU6XQ06fM+SMOVPOnLPkrDlbzp5z5Jw5V86d8+S8+QcqEArEAqlALlAK1AKtQC8wCswCq8AucArcAq/ALwgKwoKoIC5ICtKC
rCAvKArKgqqgLmgK2oKugIK+YCgYC6aCuWApWAu2gr3gKDgLroK74Cl4iw9UIpSIJVKJXKKUqCVaiV5ilJglVold4pS4JV6JXxKU
hCVRSVySlKQlWUleUpSUJVVJXdKUtCVdCSV9yVAylkwlc8lSspZsJXvJUXKWXCV3yVPylh+oQqgQK6QKuUKpUCu0Cr3CqDArrAq7
wqlwK7wKvyKoCCuiirgiqUgrsoq8oqgoK6qKuqKpaCu6Cir6iqFirJgq5oqlYq3YKvaKo+KsuCruiqfirT5QjVAj1kg1co1So9Zo
NXqNUWPWWDV2jVPj1ng1fk1QE9ZENXFNUpPWZDV5TVFT1lQ1dU1T09Z0NdT0NUPNWDPVzDVLzVqz1ew1R81Zc9XcNU/NW3+gBqFB
bJAa5AalQW3QGvQGo8FssBrsBqfBbfAa/IagIWyIGuKGpCFtyBryhqKhbKga6oamoW3oGmjoG4aGsWFqmBuWhrVha9gbjoaz4Wq4
G56Gt/lALUKL2CK1yC1Ki9qitegtRovZYrXYLU6L2+K1+C1BS9gStcQtSUvakrXkLUVL2VK11C1NS9vStdDStwwtY8vUMrcsLWvL
1rK3HC1ny9Vytzwtb/uBOoQOsUPqkDuUDrVD69A7jA6zw+qwO5wOt8Pr8DuCjrAj6og7ko60I+vIO4qOsqPqqDuajraj66Cj7xg6
xo6pY+5YOtaOrWPvODrOjqvj7ng63u4D/T+YIP4/Cnxr+Lf4fqvmt9x969S3wHwrwzekf2PxN4h+o983bH3jzTdQfBH+heYXU18w
fK34a35fu/ku+HelviL+yuY7qO/XfJj/r4cBRphghgVW2GCHA0644IYHXj5Qj9Aj9kg9co/So/ZoPXqP0WP2WD12j9Pj9ng9fk/Q
E/ZEPXFP0pP2ZD15T9FT9lQ9dU/T0/Z0/T+n7xl6xp6pZ+5ZetaerWfvOXrOnqvn7nl63v4DDQgD4oA0IA8oA+qANqAPGAPmgDVg
DzgD7oA34A8EA+FANBAPJAPpQDaQDxQD5UA1UA80A+1AN/z/nH5gGBgHpoF5YBlYB7aBfeAYOAeugXvgGXiHDzQijIgj0og8ooyo
I9qIPmKMmCPWiD3ijLgj3og/EoyEI9FIPJKMpCPZSD5SjJQj1Ug90oy0I934f1T9yDAyjkwj88gyso5sI/vIMXKOXCP3yDPyjh9o
QpgQJ6QJeUKZUCe0CX3CmDAnrAl7wplwJ7wJfyKYCCeiiXgimUgnsol8opgoJ6qJeqKZaCe66b9w+olhYpyYJuaJZWKd2Cb2iWPi
nLgm7oln4p0+0IwwI85IM/KMMqPOaDP6jDFjzlgz9owz4854M/5MMBPORDPxTDKTzmQz+UwxU85UM/VMM9POdPN/Gfczw8w4M83M
M8vMOrPN7DPHzDlzzdwzz8w7f6AFYUFckBbkBWVBXdAW9AVjwVywFuwFZ8Fd8Bb8hWAhXIgW4oVkIV3IFvKFYqFcqBbqhWahXeiW
/0vVLwwL48K0MC8sC+vCtrAvHAvnwrVwLzwL7/KBVoQVcUVakVeUFXVFW9FXjBVzxVqxV5wVd8Vb8VeClXAlWolXkpV0JVvJV4qV
cqVaqVealXalW/+veL8yrIwr08q8sqysK9vKvnKsnCvXyr3yrLzrB9oQNsQNaUPeUDbUDW1D3zA2zA1rw95wNtwNb8PfCDbCjWgj
3kg20o1sI98oNsqNaqPeaDbajW77bzj9xrAxbkwb88aysW5sG/vGsXFuXBv3xrPxbh9oR9gRd6QdeUfZUXe0HX3H2DF3rB17x9lx
d7wdfyfYCXeinXgn2Ul3sp18p9gpd6qdeqfZaXe6/b/99TvDzrgz7cw7y866s+3sO8fOuXPt3DvPzrt/oAPhQDyQDuQD5UA90A70
A+PAPLAO7APnwD3wDvyD4CA8iA7ig+QgPcgO8oPioDyoDuqD5qA96I7/ZtwfDAfjwXQwHywH68F2sB8cB+fBdXAfPAfv8YFOhBPx
RDqRT5QT9UQ70U+ME/PEOrFPnBP3xDvxT4KT8CQ6iU+Sk/QkO8lPipPypDqpT5qT9qQ7/6OhPxlOxpPpZD5ZTtaT7WQ/OU7Ok+vk
PnlO3vMDXQgX4oV0IV8oF+qFdqFfGBfmhXVhXzgX7oV34V8EF+FFdBFfJBfpRXaRXxQX5UV1UV80F+1Fd/0HVX8xXIwX08V8sVys
F9vFfnFcnBfXxX3xXLzXB7oRbsQb6Ua+UW7UG+1GvzFuzBvrxr5xbtwb78a/CW7Cm+gmvklu0pvsJr8pbsqb6qa+aW7am+7+j83+
ZrgZb6ab+Wa5WW+2m/3muDlvrpv75rl57w/0IDyID9KD/KA8qA/ag/5gPJgP1oP94Dy4D96D/xA8hA/RQ/yQPKQP2UP+UDyUD9VD
/dA8tA/d8x/i/cPwMD5MD/PD8rA+bA/7w/FwPlwP98Pz8D4f6EV4EV+kF/lFeVFftBf9xXgxX6wX+8V5cV+8F/8leAlfopf4JXlJ
X7KX/KV4KV+ql/qleWlfuvd/pOhfhpfxZXqZX5aX9WV72V+Ol/Plerlfnpf35Q97M+PE8swvWgAAAABJRU5ErkJggg==
EOF
}
