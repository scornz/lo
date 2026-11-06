# Spike: rozed on Zed's crates

What building rozed on Zed v1.27.0's crates showed (design §2, "First milestone").

## Building

rozed depends by path on 26 Zed crates (listed in the root `Cargo.toml`'s `[workspace.dependencies]`).
With what they pull in, `Cargo.lock` holds 1,104 packages, 109 of them Zed crates (all targets,
so including Linux and Windows platform crates that macOS builds skip).

What the build needs beyond Rust:

- **Xcode**, not just the Command Line Tools. gpui compiles its Metal shaders at build time.
- **cmake.** Zed's `language` crate turns on tree-sitter's `wasm` feature, which builds wasmtime's
  C API with cmake, whether or not rozed uses extensions. It's pinned in `mise.toml`. mise's shim
  fails when a build script calls cmake from outside the repo, so cargo has to run with mise
  activated or under `mise exec --`.
- **Three crate features** that only Zed's own binary or dev-dependencies turn on:
  `gpui_platform/font-kit` (without it macOS renders no text), `languages/load-grammars` (no
  grammars, so no highlighting) and `util/debug-embed` (debug builds otherwise look for Zed's assets
  under rozed's root and panic).
- **Zed's `[patch]` sections, profiles and `.cargo/config.toml`**, which cargo ignores in a
  dependency. `cargo xtask sync-zed` copies them, and seeds `Cargo.lock` with Zed's versions.

Things that surprised:

- **`--printenv`.** Zed's crates learn the login shell's environment by running the current
  executable with `--printenv` inside that shell, once at startup when there's no terminal and once
  per project folder. Before rozed handled the flag, each capture started another full rozed, which
  captured again: a runaway chain of processes. rozed now answers it as Zed does, and a rozed started
  by a running rozed refuses to start.
- **Debug links warn about unwind info.** Unoptimized code gives a debug rozed 34 MB of
  `__eh_frame`, more than the 16 MB ld's compact unwind table can index, so ld warns on every debug
  link. It's harmless, and Zed's own binary gets the same warning. Linking with `-no_compact_unwind`
  silences it but leaves out `__unwind_info`, which the unwinder needs: every panic then aborts with
  "failed to initiate panic", and one failing test kills the whole test binary. Optimized builds stay
  under the limit (`release` 7 MB, `release-fast` 11 MB) and don't warn.
- **`shallow = true` doesn't keep the submodule shallow.** With it, `git submodule update --init`
  still fetched all of Zed's history (400 MB, 55 s), because the pinned commit, on rozed's patch
  branch, isn't on the fork's default branch. `git submodule update --init --depth 1` fetches the
  pinned commit alone (32 MB, 7 s), as CI's checkout does.
- **Zed-owned files.** A run creates only rozed's folders. It also creates
  `~/Library/Logs/rozed/telemetry.log`, empty, because Zed's `Client` sets up its telemetry sink;
  nothing is sent.

## Numbers

Local, on an 18-core Apple silicon Mac with 64 GB:

| | |
|---|---|
| Clean debug build of `rozed` | 1 min 33 s (770 s of CPU) |
| Rebuild after changing `crates/rozed` | about 2 s |
| `target/` after a clean debug build | 11 GB (14 GB with clippy and test builds) |
| Debug binary | 518 MB |
| Release build (thin LTO, one codegen unit) | 6 min |
| Release binary | 139 MB, 106 MB stripped |

CI, `rust` job on `macos-26` (arm64):

| | |
|---|---|
| No cache | 10 min (fetching and building xtask 3 min, clippy 2 min, tests 3 min) |
| Cached | 8.5 min (restoring the cache 40 s, clippy 2 min, tests 4 min) |
| With `test-support` features, cache partly matching (after a `Cargo.lock` change) | 13 min (restoring the cache 45 s, clippy 3.5 min, tests 7 min) |
| With `test-support` features, cache fully matching | 5.7 min (restoring the cache 35 s, clippy 1.3 min, tests 3 min) |
| `target/` at the end | 4.6 GB, 5.3 GB with `test-support`, with 90 GB of disk still free |
| Cache saved | 1.3 GB |

rozed's gpui tests need `test-support` features on `gpui`, `project`, `settings` and `workspace`.
Features differ between test and normal builds, so tests compile Zed's crates a second time locally
too: the first `cargo test` after `cargo build` takes about a minute and adds to `target/`.

CI turns off debug info and incremental compilation, which only cost time and disk in a fresh
checkout. Nothing else was needed for disk, memory or time.

The cache holds registry and git crates only. `Swatinem/rust-cache` keeps only packages whose
manifests sit outside the repository, and Zed's crates sit inside it, in `third_party/zed`, so they
build on every run. Caching them would also need their files' modification times fixed, because
cargo decides whether a path dependency changed by modification time, and a checkout makes every
file new.

## Follow-ups

- **Quitting logs `timed out waiting on app_will_quit`**, in debug and release builds. gpui gives
  the handlers Zed's crates register for quitting (workspace and session state, project shutdown)
  200 ms, and one of them takes longer. Nothing rozed relies on is saved yet; find which before
  rozed keeps state there.
- **The window title reads "empty project"**, Zed workspace's title for a project with no folders.
