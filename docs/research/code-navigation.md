# Milestone 10 research: code features on git-sourced buffers

Zed citations are against `~/src/zed-worktrees/rozed-v1.27.0` (branch `rozed/v1.27.0`, Zed v1.27.0
plus rozed's 4 patches, HEAD `02b1c42d6f`), relative to that root. rozed citations name the branch.
Nothing was changed in either tree.

## Answer in brief

1. **Zed only gives a buffer to a language server if the buffer is a file on disk in a local
   worktree.** It must be Zed's concrete `worktree::File`, it's sent as a `file://` URI, and every
   result is opened from disk. Remote, SSH and collab projects don't change this. They forward each
   request to a host, and on that host the file is again a local worktree file. Zed has no way to
   serve a virtual document. Zed's own commit view gets no language server help on historic files
   either. So a literal "built-in language server that Zed's editor talks to" can't serve rozed's
   git-sourced buffers: both sides on untrusted repos, and the left side always.
2. **The editor's `SemanticsProvider` is a public trait with a public setter.** A crate outside Zed
   can give any editor its own provider with no patch, including both editors of a
   `SplittableEditor`. That provider can be backed by `symbol_index` and wrap Zed's project
   provider. It handles go-to-definition, hover and document highlights, and it works in the split
   diff's left editor. It can return targets in any `Entity<Buffer>`, including rozed's in-memory
   git buffers. Find-references goes around it, but rozed's `gr` panel is rozed's own UI anyway.
3. **Zed has no idea of precedence.** An editor has one provider. `LspStore` concatenates the
   answers of every server attached to a buffer. "Language server answers win" therefore has to be
   rozed code, whatever the transport.
4. **Zed has no peek.** Its go-to-definition opens a new tab, a preview tab or a "Definitions"
   multibuffer tab. A peek can be built from public API: blocks with an embedded editor, as rename
   does, plus the split diff's automatic balancing spacers. A file view in the same tab and "into
   the diff" are also rozed code on public API.
5. **Recommendation:** answer from the syntax index in rozed's own process, through a rozed
   `SemanticsProvider` that wraps Zed's (Option C below). It needs no Zed patches. One optional
   patch of about 15 lines would make it tidier. This meets every behaviour in design §7. It doesn't
   meet the mechanism sentence "runs as a built-in language server … rozed starts a copy of itself
   in a server mode", so that bullet should be reworded (proposed text at the end).

---

## 1. How Zed attaches a language server to a buffer

### The path, and its gates

1. `Editor::register_visible_buffers` (`crates/editor/src/editor.rs:11487-11498`) keeps only buffers
   that `is_lsp_relevant` accepts (`crates/editor/src/completions.rs:139-157`). The file must
   downcast to `project::File`, have an `entry_id` that exists in its worktree, and not be ignored.
2. `Editor::register_buffer` (`editor.rs:11501-11519`) → `Project::register_buffer_with_language_servers`
   (`crates/project/src/project.rs:3385-3393`) → `LspStore::register_buffer_with_language_servers`
   (`crates/project/src/lsp_store.rs:5374-5395`). This returns without registering unless
   `File::from_dyn(buffer.file())` is `Some` and `file.is_local()`. The comment at `5386-5389` says
   it outright: "we don't support non-file URI schemes in our LSP impl".
3. `LocalLspStore::register_buffer_with_language_servers` (`lsp_store.rs:3198-3213`) repeats both
   gates. It takes `abs_path` (`3215`), makes the URI with `file_path_to_lsp_url(&abs_path)`
   (`3227`), looks the worktree up by id (`3236-3245`) and walks servers by worktree, path and
   language (`3246-3268`).
4. `File::from_dyn` (`crates/worktree/src/worktree.rs:3965-3971`) downcasts `dyn language::File` to
   the concrete `worktree::File`. Any other `File` type is invisible to language servers. There are
   only two worktree kinds, `Worktree::{Local, Remote}` (`worktree.rs:103-106`).
5. The servers for a buffer are keyed by its worktree, parent path and language
   (`language_server_ids_for_buffer`, `lsp_store.rs:1500-1518`), and only running ones count
   (`language_servers_for_buffer`, `1530-1543`).
6. **Results are opened from disk.** `open_local_buffer_via_lsp` (`lsp_store.rs:10542-10640`) turns
   the URI into a file path, finds or creates an invisible worktree for it, and opens the buffer
   through the `BufferStore` from disk. The only non-file case is Yarn's `zip:` URIs, which Zed
   unzips into a temporary directory (`crates/project/src/yarn.rs:1-6`, `67-92`).

### Remote, SSH and collab

- In a remote project, `LspStore` has an `upstream_client` (`lsp_store.rs:4932-4944`).
  `request_lsp` forwards every request upstream (`6262-6270`), and so does `definitions`
  (`7223-7267`).
- On the host, the SSH headless server builds an ordinary local `WorktreeStore` and `LspStore` over
  a real `Fs` (`crates/remote_server/src/headless_project.rs:127`, `232`).
- On the guest, files are `worktree::File { is_local: false }` (`worktree.rs:3929-3963`,
  `from_proto`).
- So "files that live elsewhere" always means a real file on some machine's disk.

### Historic and in-memory buffers

- Zed's own commit view uses a `GitBlob` file whose `as_local()` returns `None`
  (`crates/git_ui/src/commit_view.rs:1070-1110`). Zed shows historic files with no language
  servers.
- rozed's `GitFile` follows the same pattern (`integration/pr-tab:crates/rozed/src/pr_diff.rs:1161-1225`;
  `feat/pr-tab` moved it into `review_ui`). Its doc comment already says "Language servers never
  see it" (`pr_diff.rs:929-930`).
- The diff's left side is `BufferDiff`'s base-text buffer. It's a `Buffer::local` with no file at
  all (`crates/buffer_diff/src/buffer_diff.rs:1618-1632`).
- The split diff's left editor turns LSP data off (`editor.disable_lsp_data()`,
  `crates/editor/src/split.rs:761`), so it never registers its buffers (`editor.rs:11488`, `11502`).

### Trust and server reuse (matters for any scheme that adds worktrees)

- A server waits to start until `TrustedWorktrees::can_trust` allows it (`lsp_store.rs:539-575`).
  Invisible worktrees are always trusted (`crates/project/src/trusted_worktrees.rs:477-481`).
- An invisible worktree reuses a visible worktree's running server for the same language
  (`reuse_existing_language_server`, `lsp_store.rs:3375-3420`).

### In-process servers aren't possible

`lsp::LanguageServer::new` (`crates/lsp/src/lsp.rs:516`) spawns a binary. The stream-based
`new_internal` is private (`lsp.rs:584`). A language server is always a child process.

### Workarounds I rejected

- **A fake `worktree::File`.** Its fields are all public (`worktree.rs:3824-3831`), so rozed could
  stamp an in-memory buffer as a file in a local worktree. It doesn't hold up:
  - `is_lsp_relevant` needs a real worktree entry (`completions.rs:146-156`), so the editor never
    registers the buffer.
  - Every result is reopened from disk (`lsp_store.rs:10542+`).
  - Buffers from two commits would claim the same path.
- **A virtual worktree, or materialised blobs.** See §3.

---

## 2. `SemanticsProvider`: can rozed plug in its own?

Yes, with no patch.

- **The trait** is at `editor.rs:11875-11957`. Its methods:
  - `hover`, `debugger_hover` (defaults to `None`)
  - `inline_values`, `applicable_inlay_chunks`, `invalidate_inlay_hints`, `inlay_hints`,
    `supports_inlay_hints`
  - `semantic_tokens`, `supports_semantic_tokens`
  - `document_highlights`
  - `definitions(kind)`, where `kind` is `Symbol`, `Declaration`, `Type` or `Implementation`
  - `range_for_rename`, `perform_rename`

  Every type in its signatures is public: `InvalidationStrategy` and `CacheInlayHints`
  (`project.rs:172`, `lsp_store.rs:162-167`), `RenameTarget` (`editor.rs:1611`), `LocationLink`
  (`project.rs:909-912`) and `Location` (`crates/language/src/language.rs:311-314`).
- **Setting it.** The field is at `editor.rs:1010`. `Editor::new` defaults it to
  `Rc::new(project.downgrade())` (`2401-2403`). The getter and setter are public (`3311-3317`), and
  nothing else in Zed writes the field.
- **Wrapping Zed's provider.** `impl SemanticsProvider for WeakEntity<Project>` is public
  (`editor.rs:11959+`), so a rozed wrapper can forward to it.
- **Precedent.** Zed's own tests swap in custom providers (`crates/editor/src/hover_popover.rs:1959`,
  `2281`, `2355`).

**What uses the provider:**

- go-to-definition: `go_to_definition_of_kind` (`crates/editor/src/navigation.rs:2394-2441`) and
  `definition_locations_of_kind` (`1317-1347`)
- hover (`hover_popover.rs:236-261`)
- cmd-hover links (`crates/editor/src/hover_links.rs:295`, `539`)
- document highlights (`editor.rs:3759-3765`)
- semantic tokens (`crates/editor/src/semantic_tokens.rs:79`)
- inlay hints (`crates/editor/src/inlays/inlay_hints.rs:1335`)
- rename (`editor.rs:8164`)

**What goes around it:**

- **Find all references** calls `project.references` directly (`navigation.rs:1363`, `1410`).
  rozed's `gr` panel is its own UI, so this costs nothing.
- **Workspace symbols and completions** have their own paths (`completion_provider`,
  `editor.rs:2400`). Design §7 needs neither.
- **The file outline** comes from tree-sitter unless the `document_symbols` setting uses LSP
  (`crates/editor/src/document_symbols.rs:25-55`). The default is `"off"`
  (`assets/settings/default.json:1458`). So the outline already works on both sides of any repo,
  with no index.

**The left editor.** `disable_lsp_data` (`split.rs:761`) gates registration, inlays, semantic tokens,
code lens and the cmd-hover underline (`hover_links.rs:253-311`). It does **not** gate
go-to-definition (`navigation.rs:2394-2414`), hover (`hover_popover.rs:236-261`) or document
highlights (`editor.rs:3759`). So a provider on the left editor gives `gd`, `K` and highlights
there.

**Wiring it into `SplittableEditor`:**

- The right editor is built once in `SplittableEditor::new` (`split.rs:633`) and exposed through
  `rhs_editor()` (`561`).
- The left editor is built in `split()` (`744-761`). It's rebuilt every time the diff splits again:
  on `toggle_split` (`1038`), and automatically when the pane widens past the minimum width
  (`1421-1431`). `split()` ends with `cx.notify()` (`938`).
- **No patch:** rozed observes the `SplittableEditor` and sets its provider whenever `lhs_editor()`
  (`565`) returns a new entity.
- **Optional patch:** `SplittableEditor::set_semantics_provider`, remembered for later left editors.
  It would be about 15 lines, the same shape as patch `02b1c42d6f` (`disable_expand_excerpt_buttons`,
  20 lines).

**Telling the provider which commit a buffer belongs to:**

- **Right side, in-memory.** Downcast `buffer.file()` to `GitFile`. `language::File` is `Any`
  (`crates/language/src/buffer.rs:358`).
- **Right side, trusted checkout.** A project buffer: its worktree id maps to the checkout, and the
  checkout to the head.
- **Left side, either of:**
  - keep a map from `BufferId` to (tree, path), or
  - build the base buffer with a `GitFile` and pass it to `BufferDiff::new_with_base_text_buffer`
    (`buffer_diff.rs:1645-1657`), or set the file later with `Buffer::file_updated`
    (`buffer.rs:1795`).

**Targets** go back as `Location { buffer, range }` and can point at any buffer entity. rozed should
keep a registry from (tree, path) to buffer that reuses the diff's own buffers for changed files.
Then "into the diff" (§5) becomes a simple identity check.

---

## 3. A virtual or read-only worktree over the bare clone

It's possible without a patch:

- `Project::local` takes `fs: Arc<dyn Fs>` (`project.rs:1286-1302`).
- Worktrees read everything through it (`Worktree::local`, `worktree.rs:485-498`). Worktree code
  never calls `std::fs` directly.
- `Fs` is a public trait with 39 methods (`crates/fs/src/fs.rs:104`).

A wrapper would delegate to `RealFs` and serve `…/rozed-virtual/<repo>/<tree>/…` from
`git ls-tree -r -l` and `cat-file --batch`. Trees are local in rozed's `blob:none` clone
(`repo_cache/src/cache.rs:16`, `207`), and blobs are fetched on demand. Zed would then open real
project buffers at any commit.

**What it buys:** only rozed's own server can read it. Real language servers read the real disk,
and the design keeps them on the trusted head checkout anyway.

**What it costs:**

- **The virtual `Fs` itself.** About 400 lines plus tests. It also tracks a 39-method trait that
  changes between Zed releases; `trash` and `restore` with `TrashId` are recent additions.
- **Worktrees.** One per tree on screen: base and head for each PR, and more for revision
  comparisons and rebase-aware merge-tree results. Each one scans its tree into a snapshot.
- **Buffer swaps.** Every diff buffer has to become a project buffer, the way checkout buffers
  replace git buffers today (`integration/pr-tab:crates/rozed/src/pr_diff.rs:240-320`). The left
  side's base buffers would come from the virtual worktree through `new_with_base_text_buffer`, and
  rozed would have to register them by hand (`project.rs:3385`), because the left editor never
  registers its buffers.
- **A trust and visibility trap:**
  - If the virtual worktree is invisible, it reuses the visible trusted checkout's servers
    (`lsp_store.rs:3375-3420`). rust-analyzer would then receive old-side files.
  - If it's visible, its servers wait for trust (`lsp_store.rs:539-575`;
    `trusted_worktrees.rs:461-498`). rozed would have to trust each one (`trusted_worktrees.rs:280`).
  - rozed would also have to set `language_servers` per worktree
    (`SettingsStore::set_local_settings`, `crates/settings/src/settings_store.rs:1064`), because its
    forced layer is global. Without trust it forces `"language_servers": []`
    (`feat/pr-tab:crates/rozed/src/forced_settings.rs:44`, `97-102`).
- **Processes.** A `rozed --lsp` process for each worktree and server: two or more per PR, each
  loading its tree's index. `symbol_index`'s `Store` lives in memory and "never evicts", so each
  process either rebuilds it or the crate gains a disk cache. In one process, one `Store` per repo is
  shared by every tab.
- **Still needed on top:** the precedence logic (§4), the peek (§5) and "into the diff" (§5).

**A variant from Zed's Yarn precedent:** write raw blobs into a cache directory as they're needed,
with no checkout and no filters. The changed files of both sides would be written when the PR opens,
and the server would write each target before replying. These are real files, so there's no virtual
`Fs`. The variant keeps the same per-commit worktree, trust, settings, process and precedence costs.
It also blurs design §3/§7's "checkouts for trusted repos": nothing runs, but an untrusted repo's
files land on disk.

**Verdict:** possible, but it's a large, fragile layer whose only reader would be rozed's own
server.

---

## 4. Precedence

- An editor has exactly one provider (`editor.rs:1010`), with no chaining.
- Inside Zed's project provider, `LspStore` asks every server attached to the buffer and joins their
  answers:
  - definitions: `flat_map` then `dedup` (`lsp_store.rs:7268-7284`)
  - hovers: all of them, deduplicated (`9359-9374`)
  - both go through `request_filtered_lsp_locally` (`10720-10760`)
- With two answers, go-to-definition opens a "Definitions for X" multibuffer tab
  (`navigation.rs:1784-1842`).
- So a literal second server on a trusted repo would show syntax answers next to rust-analyzer's,
  not underneath them. Changing that through settings would mean editing per-worktree
  `language_servers` while servers run, which restarts them.

**Rule for rozed's provider (`RozedSemantics`):**

1. **Left side, untrusted repos, or a buffer with no language server running:** answer from the
   index.
2. **Trusted right side:** send the language-server request through the wrapped project provider,
   and compute the index answer at the same time.
   - If the server is **ready** and returns locations, they win.
   - If the server is **starting**, or errors or times out, use the index answer, labelled
     "syntax" in the peek. Design §11 says syntax navigation carries on when a server crashes or
     hangs.
3. **Signals for "starting" versus "ready":**
   - whether a server is running for the buffer: `LspStore::running_language_servers_for_local_buffer`
     and `language_servers_for_local_buffer` (`lsp_store.rs:9939`, `9961`)
   - `Project::language_server_statuses` (`project.rs:4420`), whose
     `LanguageServerStatus.pending_work` (`lsp_store.rs:4780-4792`) is non-empty while
     rust-analyzer indexes or primes its cache
4. **Hover (`K`):** the language server's only. On untrusted repos, either nothing or the index's
   definition signature (open question 3).
5. **Everything else** (inlay hints, semantic tokens, rename): forward unchanged. rozed is read-only,
   so rename never applies.

**Open question:** when a ready server returns nothing, which is common because dependencies are
never installed, should rozed show nothing (or the import, per design §7 "Dependencies")? Or should
it fall back to a name match, which may point at the wrong definition? I'd show the server's empty
answer and offer "search by name" in the peek.

---

## 5. Peek and navigation

**What Zed does today:**

- **No peek.** Nothing in the editor crate implements one. A code lens's `peekLocations` command
  just opens a multibuffer (`crates/editor/src/code_lens.rs:86-98`). Vim binds `g d` to
  `editor::GoToDefinition` (`assets/keymaps/vim.json:99`).
- **Go-to-definition:** `go_to_definition` (`navigation.rs:1039-1064`) → `go_to_definition_of_kind`
  (`2394-2441`) → `navigate_to_hover_links` (`1709-2045`):
  - several locations: a "Definitions for X" multibuffer tab (`1784-1842`)
  - one location in the same singleton buffer: moves the cursor (`1904-1943`)
  - otherwise: `workspace.open_project_item` opens the target in the active pane (`1945-2039`).
    That reuses an open item, or makes a new tab or preview tab
    (`crates/workspace/src/workspace.rs:5578-5650`).

  It never shows a definition in place. cmd-click takes the same path (`hover_links.rs:451`).
- **Nav history** is kept per pane:
  - Editors push entries through `ItemNavHistory` (`crates/workspace/src/pane.rs:466-469`,
    `4771-4802`).
  - `pane::GoBack` and `GoForward` are bound to `ctrl-o` and `ctrl-i` in vim (`vim.json:67-68`) and
    by rozed's PR tab (`feat/pr-tab:crates/review_ui/src/pr_tab.rs:64-65`). They call
    `workspace.go_back` and `go_forward` (`workspace.rs:3402-3418`), which activate the entry's item
    and call `Item::navigate(data)` (`crates/workspace/src/item.rs:221-228`).
  - The editor's `NavigationData` is `pub(crate)` (`editor.rs:1664-1669`): rozed can't read it, but
    it can push data of its own type.
  - `SplittableEditor::navigate` forwards to its focused editor (`split.rs:2053-2061`), and rozed's
    `PrTab` already forwards `navigate` and nav history to the diff (`pr_tab.rs:1335-1355`).

**Peek, built on public API:**

- **The block.** Insert a block under the cursor's line with `Editor::insert_blocks`
  (`editor.rs:8881`; `BlockProperties` at `crates/editor/src/display_map/block_map.rs:282-290`).
  Rename already embeds a whole `Editor` in a block this way (`editor.rs:8298-8330`).
- **Its contents.** A read-only `Editor` over a `MultiBuffer` of the definition excerpts, ranked
  same file, then same directory, then import. Give it `RozedSemantics` too, so `gd` works inside
  the peek.
- **Split alignment.** A block on one side of the split automatically gets a spacer of the same
  height on the other side (`balancing_block`, `block_map.rs:1751-1795`; applied around
  `crates/editor/src/display_map.rs:500-530`). The two sides stay aligned with no extra work.
- **Keys.** rozed binds `gd` in a context more specific than vim's, so it overrides
  `editor::GoToDefinition` there.

**Full file in the same tab:** `PrTab` swaps its content between the diff and a read-only `Editor`
on the target buffer.

- On a trusted head, the target is a checkout project buffer, so language servers keep working
  inside it. Everywhere else, it's an in-memory git buffer.
- At each switch, `PrTab` pushes its own nav entry (`ItemNavHistory::push`).
- `PrTab::navigate` restores the view for rozed's own entries, and passes any other entry to
  whichever editor is showing.
- `⌃o` and `⌃i` then work across the switch with no change to Zed.

**Into the diff:** applies when the target is a file the PR changes, on the same side.

- **Right side.** The buffer registry returns the diff's own buffer, so that's an identity check.
  1. `MultiBuffer::buffer_point_to_anchor` (`crates/multi_buffer/src/multi_buffer.rs:1939`).
  2. If the point is in a collapsed, unchanged region, add a range around it with
     `SplittableEditor::update_excerpts_for_path` (`split.rs:1207`).
  3. Select it with `change_selections(...nav_history(true))`, which rozed already does at
     `pr_tab.rs:714`.
- **Left side.** Map the old point to the new with `base_text_point_to_buffer_point`
  (`buffer_diff.rs:825`) to expand the right-hand excerpt, then select in the left editor.

---

## Options compared

| Option | What it is | Zed patches | Satisfies the behaviours? | Main costs |
|---|---|---|---|---|
| **A. Literal, with a virtual `Fs`** | Project buffers on per-tree virtual worktrees; `rozed --lsp` registered as an `LspAdapter` (`crates/language/src/language_registry.rs:332`) | 0 | Only with rozed's peek, precedence and into-the-diff code added on top | §3: virtual `Fs`, worktree per tree, trust and settings plumbing, a process per tree with its own index, precedence still needs a custom provider |
| **B. Literal, materialised blobs** | Same, with raw blobs written to disk on demand | 0 | Same as A | A's costs minus the `Fs`, plus untrusted files on disk |
| **C. In-process provider (recommended)** | `RozedSemantics` wraps `WeakEntity<Project>`, answers from `symbol_index` in the app, and is set on both diff editors, the peek and file-view editors | 0 (optionally 1, about 15 lines) | Yes: both sides, every repo, server wins on trusted repos, peek | A provider of about 400 lines, a code-index service, and the peek, file-view and references UIs (needed under every option) |
| **D. Provider plus a `rozed --lsp` child** | C, but the provider sends LSP over stdio to a child process it spawns with `lsp::LanguageServer::new` (`lsp.rs:516`), using rozed's own URIs | 0 | Yes | C's costs plus inter-process messages, a second copy of the index and process lifecycle. It buys crash isolation, which matters little: Zed already parses everything with tree-sitter in process |

## What would and wouldn't satisfy design §7's wording

| §7 says | A or B | C | D |
|---|---|---|---|
| Syntax navigation "on every repo", by name across the whole repo | yes | yes | yes |
| "The right side navigates the new commit, and the left side the old one" | yes, but only with hand registration and a virtual or written tree for every commit on screen | yes | yes |
| "Language server answers win. Syntax-based answers fill in while a language server starts up" | no on its own (Zed joins the answers, §4); needs a custom provider or settings swaps | yes | yes |
| "Peek …", "Into the diff", `⌃o`/`⌃i` | rozed UI in every option | same | same |
| "The index runs as a built-in language server: rozed starts a copy of itself in a server mode" | yes | **no** (it runs in process) | yes |
| "Zed's editor talks to it like any other language server" | yes, for project buffers only | **no** (the editor calls rozed's provider) | **no** (rozed's provider talks to it) |
| "so this needs no changes to Zed" | yes | yes | yes |
| decisions.md "both the app and rozed's language-server mode use it [`symbol_index`]" | yes | the app only | yes |

The design's goal is navigation everywhere with no changes to Zed. Read that way, C meets it with the
least machinery. Read literally, only A or B meet the mechanism sentence, at the cost in §3.

## Recommendation

Use **Option C**:

- **`CodeIndex` service.** One per repo, holding a `symbol_index::Store`, plus one `Index` per tree.
  - It's keyed by tree, not commit, so rebase-aware merge-tree results work too.
  - Grammars come from Zed's `languages`, and indexes are built in the background.
  - It reads blobs from `repo_cache` and fetches missing blobs in batches using `Store::missing`.
- **`RozedSemantics`.** A `SemanticsProvider` that wraps `Rc<dyn SemanticsProvider>` from
  `WeakEntity<Project>`. It answers definitions from the index with the §4 rule, forwards
  everything else, and resolves targets through a (tree, path) → buffer registry that reuses the
  diff's buffers.
- **Wiring.** Set it on the right editor at creation, and on each new left editor by observing the
  `SplittableEditor`. Optionally carry the ~15-line Zed patch instead.
- **Peek.** A block with an embedded read-only editor. rozed's `gd` overrides vim's.
- **`PrTab` file view.** The same-tab full file, rozed's own nav entries, and "into the diff".
- **`gr` panel.** `project.references` when the language server is ready, otherwise
  `Index::references`.
- **`editor::GoToDefinition` and cmd-click.** These still reach Zed's navigation and would open a
  read-only tab. Capture the action, like the other palette actions, and accept or patch the
  cmd-click path (open question 5).

**Proposed rewording of design §7, "How the editor uses it":**

> **How the editor uses it.** rozed answers the editor's go-to-definition, references and highlight
> requests itself, from the index, inside the app, through the editor's hook for custom providers.
> On trusted repos it asks the language server first. This needs no changes to Zed.

**Proposed decisions.md entry:**

> The index is served in process through a `SemanticsProvider`, not as a language server. Zed sends
> only on-disk worktree files to language servers (`lsp_store.rs:5374-5395`,
> `worktree.rs:3965-3971`), and rozed's diff buffers are in memory, so a server would need a
> virtual or written-out worktree for every commit. Zed also joins every server's answers, so
> precedence needs rozed code anyway.

**Suggested PR slices:**

- **10a.** `CodeIndex` service.
- **10b.** `RozedSemantics`, wired into both sides (`gd` through Zed's path, highlights).
- **10c.** Peek.
- **10d.** File view, nav history, into the diff.
- **10e.** `gr` panel.
- **10f.** Trust gate and the language-server precedence rule.

## Open questions for the overseer

1. Reword design §7's mechanism bullet as above, dropping "a copy of itself in a server mode"? Or
   keep the literal wording and pay for Option A or B, or D?
2. When a ready language server returns no definition, show nothing (or the import), or fall back to
   the syntax answer with a label?
3. On untrusted repos, should `K` show the index's definition signature, or nothing, since the design
   lists type hovers as a language-server feature?
4. Carry the optional `SplittableEditor::set_semantics_provider` patch (about 15 lines, Zed patch
   5), or rely on observing the editor?
5. cmd-click go-to-definition calls `navigate_to_hover_links` directly (`hover_links.rs:451`) and
   opens a read-only tab for the target. Accept that for the mouse, or add a small patch to route it
   to rozed's peek?
