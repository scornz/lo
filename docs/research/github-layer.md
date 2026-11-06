# GitHub layer research for rozed (milestone 3)

Researched 2026-11-06 for the `github` crate: HTTP, credentials, SQLite, typed GraphQL, device flow,
`gh` import, polling and fixtures. Design: §3, §11 "GitHub", §12 "GitHub API".

- **Zed** is `~/src/zed` at `v1.27.0` (`c84af5b9`), unchanged. Zed paths are relative to its root.
  Its pinned reqwest fork (`zed-industries/reqwest@33bc764`) was read from `~/.cargo/git/checkouts`.
- **GitHub docs** were read from the `github/docs` source (`main`, 2026-11-06); URLs below are the
  published pages. `cli/cli` is at `v2.103.3`, `cli/oauth` at `fe4619f`.
- **Live checks** (2026-11-06, 00:27–00:45 PST): device-flow endpoints with rozed's public client ID
  (no code was approved, nothing was granted), and read-only REST and GraphQL GETs on
  `rozed-dev/playground` with rozed's OAuth token from `~/.config/rozed-dev/oauth-token`. No token
  or device code was printed or written outside a mode-600 scratch file.
- **A compile experiment** built `graphql_client`, `cynic` and `apollo-compiler` against GitHub's
  real schema with Rust 1.98.1 (scratchpad `gqlexp/`).
- **Marks:** **[observed]** means I ran it. **[inferred]** means I reasoned it out and didn't verify.
  Everything else was read in source or docs.

## Recommendations

1. **HTTP:** the `github` crate takes `Arc<dyn http_client::HttpClient>` (the app's shared,
   Zed-host-refusing client from the spike). Set `RedirectPolicy` explicitly on every request:
   without it, Zed's client follows up to 10 redirects. Fetch private-repo images in two hops by
   hand (token to `github.com`, no token to S3).
2. **Credentials:** implement Zed's `credentials_provider::CredentialsProvider` twice: Keychain
   (through GPUI's `cx.read_credentials`) in release, and a mode-600 file
   `~/.config/rozed/credentials.json` in debug. Don't use `zed_credentials_provider`: its dev file
   isn't mode 600. Key each token by a per-account server string, `rozed://github.com/<login>`,
   because Zed's Keychain items are looked up by server alone.
3. **SQLite:** use `sqlez` with rozed's own `Domain` and its own file,
   `database_dir()/0-<channel>/rozed.sqlite`. `rusqlite` can't be added beyond 0.32.x: both link
   `sqlite3`, and Zed pins `libsqlite3-sys` 0.30.1.
4. **GraphQL:** use `graphql_client` 0.16 (derive) over `.graphql` files, plus a unit test that
   validates every query against the vendored schema with `apollo-compiler`.
   - `graphql_client` alone misses argument typos and argument types.
   - Vendor `docs.github.com/public/fpt/schema.docs.graphql` (1.56 MB) with a refresh script.
     `octokit/graphql-schema` stopped updating in February 2025 and has no stacks.
5. **Device flow:** every error comes back as HTTP 200 with an `error` field. `slow_down` is
   cumulative (+5 s each time) and returns the new `interval`; use it. Recommended loop below.
6. **`gh` import:** list accounts with `gh auth status --json hosts`, which never includes tokens
   without `--show-token`. Fetch each token with `gh auth token --hostname github.com --user <login>`
   through a pipe, with `GH_TOKEN`/`GITHUB_TOKEN` removed from the child's environment.
   - Find `gh` explicitly: a Finder-launched app's `PATH` lacks Homebrew.
   - `gh` tokens lack the `workflow` scope by default.
   - Tokens can be expiring ("refreshable"; present in gh v2.103.3). Re-import on a 401.
7. **Polling:** conditional REST GETs cost nothing when they return 304 **[observed on 18
   endpoints]**. GraphQL has no ETags and costs at least 1 point per call.
   - Each tick, run a free conditional `GET /pulls/{n}`. Every tick or every few ticks, run a
     1-point GraphQL "fingerprint".
   - Fetch details only on change. Poll the merge queue through GraphQL, the only API that has it.
   - The budget (5,000/h REST, 5,000 points/h GraphQL) is per user, shared with `gh` and agents.
8. **Fixtures:** an in-process `ReplayHttpClient` (implements `HttpClient`) serves recorded
   "cassettes". It is keyed by method + path + sorted query, or for GraphQL by operation name +
   canonical variables, and returns each key's responses in order.
   - A recorder wraps the real client, keeps an allowlist of headers, and fails if the token's
     bytes appear in any file.
   - Device-flow fixtures are synthetic.

---

## 1. HTTP: Zed's `http_client`

### The trait and its types

`crates/http_client/src/http_client.rs`:

- **`HttpClient`** (`:122-174`) is `'static + Send + Sync`.
  - Required: `user_agent()`, `proxy()`, and
    `send(http::Request<AsyncBody>) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>>`.
  - Provided: `get(uri, body, follow_redirects)` and `post_json(uri, body)`.
  - `as_fake()` exists only under the `test-support` feature.
- **Requests are plain `http` 1.x types.** The crate re-exports `http`, `Request`, `Response`,
  `Method`, `StatusCode`, `Uri` and `Builder` (`:9`). Zed's workspace pins `http = "1.1"`
  (`Cargo.toml:647`).
- **Per-request options are request extensions,** added through the `HttpRequestExt` trait on
  `http::request::Builder` (`:32-68`):
  - `.follow_redirects(RedirectPolicy::{NoFollow, FollowLimit(n), FollowAll})`
  - `.timeout(Duration)`, a total deadline including the body
  - `.when(..)` and `.when_some(..)`
- **`AsyncBody`** (`async_body.rs:14-25`) is `Empty`, `Bytes(Cursor<Bytes>)` or
  `AsyncReader(Pin<Box<dyn AsyncRead + Send + Sync>>)`.
  - It converts from `()`, `Bytes`, `Vec<u8>`, `String`, `&'static str` and `Json<T>`
    (`:54-103`). `from_reader` makes a streaming body (`:36-41`).
  - Responses always come back as a streaming `AsyncReader` (below). Read them with
    `futures::AsyncReadExt::read_to_end`.
- **Wrappers.**
  - `HttpClientWithProxy` (`:177-228`).
  - `HttpClientWithUrl` (`:231-368`), a base URL plus Zed URL builders. rozed doesn't need it.
  - `BlockedHttpClient` (`:392-426`).
  - `read_proxy_from_env()` reads `ALL_PROXY`, `HTTPS_PROXY` and `HTTP_PROXY` (`:370-384`).
- **Zed's own GitHub helpers.** `http_client/src/github.rs:143-152` and
  `git_hosting_providers/src/providers/github.rs:136-142` send `Authorization: Bearer $GITHUB_TOKEN`
  when that env var is set.
  - **[inferred]** If a developer launches rozed with `GITHUB_TOKEN` exported, Zed's
    language-server download code sends that token to `api.github.com`. That's harmless, but it
    isn't rozed's account. Don't rely on it, and don't export it in tests.

### The concrete client and how requests run

`crates/reqwest_client/src/reqwest_client.rs`, on the `zed-reqwest` fork `0.12.15-zed`
(`Cargo.toml:865-871`; features `http2`, `macos-system-configuration`, `rustls-tls-native-roots`,
`socks` and `stream`, but **not** `gzip`):

- **Construction.**
  - Zed builds one client with
    `ReqwestClient::proxy_and_user_agent(proxy, "Zed/<ver> (macos; aarch64)")` inside
    `Tokio::handle(cx).enter()` (`crates/zed/src/main.rs:502-515`).
  - It sets `cx.set_http_client(Arc::new(http))` (`gpui/src/app.rs:2054-2062`).
  - Builder defaults (`:33-50`): rustls, a 10 s connect timeout, keepalives, no read timeout.
- **Where requests run.** `send` (`:268-321`) converts the `http::Request` to a reqwest request,
  then `handle.spawn`s `request.send()` on a Tokio runtime:
  - Zed's `gpui_tokio` handle if one is current.
  - Otherwise its own one-worker runtime (`:123-147`).

  The returned future just awaits the join handle, so it can be awaited from anywhere: a
  `cx.background_spawn` task (GPUI's background executor, `gpui/src/executor.rs:103-109`) or a
  foreground `cx.spawn`. **Decode large JSON on the background executor.**
- **Cancellation.** Dropping the future aborts the request (`defer(abort)`, `:299-301`). Dropping a
  GPUI `Task` that awaits a request cancels it, which is what a closed tab's poller wants.
- **Response bodies** are streamed (`bytes_stream().into_async_read()`, `:311-317`). The headers
  are copied over unchanged.
- **Errors.** Error URLs are redacted only for `key=` query params (`:249-257`). Tokens go in
  headers, never in URLs, so that's fine.

### Building a GitHub request (sketch)

```rust
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, RedirectPolicy, Request};

fn api_request(method: Method, url: &str, token: &Token, body: AsyncBody) -> anyhow::Result<Request<AsyncBody>> {
    Ok(Request::builder()
        .method(method)
        .uri(url)
        .header("Authorization", format!("token {}", token.expose())) // `token`, as gh does; uploads need it
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")                  // pin; the response echoes x-github-api-version-selected
        .header("User-Agent", concat!("rozed/", env!("CARGO_PKG_VERSION"))) // GitHub requires one; don't rely on the shared client's
        .follow_redirects(RedirectPolicy::FollowLimit(3))               // always set it explicitly (see Redirects)
        .timeout(Duration::from_secs(30))                               // GitHub gives up after 10 s server-side
        .body(body)?)
}
```

`GithubClient::new(http: Arc<dyn HttpClient>, ..)` gets `cx.http_client()` in the app, which the
spike wraps to refuse Zed's hosts, and a `ReplayHttpClient` in tests (section 8). The client core
(request building, ETags, decoding) needs no `gpui`, so it can be tested with plain `#[test]` and
`futures::executor::block_on`. The poller and the credential store live beside it and use `gpui`.

### Redirects

- **Without the extension, requests follow redirects.** `RedirectPolicy`'s `#[default]` is
  `NoFollow` (`http_client.rs:20-26`), but `ReqwestClient::send` only sets a per-request policy
  when the extension is present (`reqwest_client.rs:279-285`). Otherwise reqwest's client default
  applies: `Policy::limited(10)` (`zed-reqwest src/async_impl/client.rs:232`, `src/redirect.rs:155-160`).
  **Set the extension on every request.**
- **Auth is stripped across hosts.** On a redirect to a different host or port, reqwest removes
  `Authorization`, `Cookie`, `Proxy-Authorization` and `WWW-Authenticate`
  (`src/redirect.rs:234-246`, called at `client.rs:2954`). A followed redirect from
  `github.com/user-attachments/assets/<id>` to S3 therefore does **not** leak the token.
  - **Still, do that hop by hand:** `NoFollow`, check the `Location` host (an
    `*.s3.amazonaws.com` host), then GET it with no `Authorization`. That is defense in depth
    against a future client change, and it lets rozed cache the signed URL. The URL is valid for
    5 minutes (`X-Amz-Expires=300`, `research/github-checks.md` §3).
- **Methods and bodies.**
  - 301/302/303 drop the body and switch non-GET methods to GET (`client.rs:2867-2886`).
  - 307/308 are followed only if the body can be replayed. An `AsyncBody::from_reader` body
    can't, so the 307 comes back as the response (`client.rs:2887-2890`).
- **API redirects.** The REST API answers a renamed repo with a 301 to the same host. Following
  up to 3 same-host redirects with the token kept is right for API calls.

### Uploads (attachments)

- **Use a `Bytes` body,** `AsyncBody::from(Vec<u8>)`. Files are at most 10 MB for images and
  100 MB for videos (`research/github-api.md` §1). The body is replayable on a 307/308, and reqwest
  sets `Content-Length` itself. gh's PR #14180 says the upload endpoint requires a content length.
- **A streaming body** (`from_reader` → `reqwest::Body::wrap_stream`, `reqwest_client.rs:292-294`)
  is sent chunked unless you add `Content-Length` by hand **[inferred from hyper's behaviour]**, and
  can't follow a 307. Only use it with an explicit `Content-Length` and `NoFollow`.
- **Timeout.** Give uploads a longer `.timeout` (minutes, not 30 s).

### Testing support in Zed

- **`FakeHttpClient`** (feature `test-support`, `http_client.rs:428-521`).
  - `FakeHttpClient::create(|req| async { Ok(Response) })` returns `Arc<HttpClientWithUrl>`, which
    coerces to `Arc<dyn HttpClient>`.
  - Also `with_404_response()`, `with_200_response()`, and `replace_handler` to chain handlers.
  - Zed's tests build `AppState::test` on it (`research/zed-crates.md` §1).
- **For rozed:** a small `ReplayHttpClient` implementing the three trait methods is clearer than
  closures (section 8). It needs no feature flag.

---

## 2. Credentials

### What Zed has

| Piece | What it does | Where |
|---|---|---|
| `credentials_provider::CredentialsProvider` | Trait: `read_credentials(url, &AsyncApp) -> Option<(username, Vec<u8>)>`, `write_credentials(url, username, password)`, `write_credentials_if_not_exists`, `delete_credentials`. Futures are `!Send` (they take `&AsyncApp`). | `crates/credentials_provider/src/credentials_provider.rs:14-63` |
| `FakeCredentialsProvider` | In-memory map, can fail reads, defer reads and mutations, and record reads. Feature `test-support`. | `crates/credentials_provider/src/test_support.rs:8-124` |
| `zed_credentials_provider` | Picks `DevelopmentCredentialsProvider` when `ReleaseChannel::Dev` and `ZED_DEVELOPMENT_USE_KEYCHAIN` is unset, else `KeychainCredentialsProvider`. | `crates/zed_credentials_provider/src/zed_credentials_provider.rs:22-24`, `45-66` |
| Keychain provider | Delegates to `cx.read_credentials` / `write_credentials` / `delete_credentials`. | same file `:69-116`; `gpui/src/app.rs:1876-1912` |
| macOS implementation | Runs on the background executor (`gpui_macos/src/platform.rs:1224-1258`). Uses **internet-password** items (`kSecClassInternetPassword`) with `kSecAttrServer = url`, `kSecAttrAccount = username` and `kSecValueData = password`. | `crates/gpui_apple/src/keychain.rs` |
| Dev file provider | One JSON file `paths::config_dir()/development_credentials`, a map of `url → (username, bytes)`, written with `std::fs::write`. | same file `:125-196` |

### Details that matter for rozed

- **One item per server string.**
  - `write_credentials` runs `SecItemUpdate` with the query `{class, server}` and, if nothing
    matches, `SecItemAdd` (`keychain.rs:30-64`).
  - `read_credentials` and `delete_credentials` match on `{class, server}` only
    (`keychain.rs:102-180`).

  So two GitHub accounts under the same server string would overwrite each other. **Put the login
  in the server string.**
- **The dev fallback exists but isn't usable as is.** It's chosen only for `ReleaseChannel::Dev`
  (a debug build of Zed). `std::fs::write` creates the file with the process umask, normally 0644
  **[inferred]**, not 0600. The file is also shared by everything that uses the provider.
- **Keychain prompts on rebuilds.** Unsigned and ad-hoc-signed rebuilds change the code identity
  in the item's access list, so macOS prompts on each rebuild (handoff "Notes"). That's why Zed
  defaults to the file in dev (comment at `:47-55`).
- **The GPUI test platform's Keychain is a no-op:** `write` → `Ok`, `read` → `None`
  (`gpui/src/platform/test/platform.rs:813-823`). Tests must use `FakeCredentialsProvider` or a
  temp-dir file store.

### Recommended design

- **`TokenStore`** = `Arc<dyn credentials_provider::CredentialsProvider>`, chosen at startup. Using
  Zed's trait gets `FakeCredentialsProvider` for tests for free.
  - **Release:** `KeychainTokenStore`, which calls `cx.read_credentials(server)` and so on.
  - **Debug** (`cfg!(debug_assertions)`, with `ROZED_USE_KEYCHAIN=1` to force the Keychain, like
    Zed's env var): `FileTokenStore` at `paths::config_dir()/credentials.json`, which is
    `~/.config/rozed/credentials.json` once the spike's `APP_NAME` patch is in.
- **Server string:** `rozed://github.com/<login>`. A host-qualified login keeps GHES and ghe.com
  open for later. Username: `<login>`. Password: the token bytes.
  - **[inferred]** A `rozed://` server never matches Safari's `github.com` web passwords, so it won't
    show up in AutoFill.
- **File store rules.**
  - Create it with `OpenOptions::new().mode(0o600)` (`std::os::unix::fs::OpenOptionsExt`), write a
    temp file in the same directory, then `rename` over the old one.
  - On read, if the mode isn't 0600, `chmod` it back and log a warning.
  - Directory `~/.config/rozed` at 0700.
  - Never log the contents.
- **The account list lives in SQLite,** not the Keychain: login, user id, source (device flow or
  `gh`), scopes and the default account per owner. The Keychain has no enumeration API in Zed's
  wrapper, and the list isn't secret.
- **In memory,** hold tokens in a newtype with a redacting `Debug` and no `Display`. Zeroize on drop
  if it's cheap (the `zeroize` crate isn't in Zed's tree; plain `String` is acceptable).

---

## 3. SQLite: `sqlez` and `db`

### What they are

- **`sqlez`** (`crates/sqlez`) is a thin, unsafe wrapper over `libsqlite3-sys` with typed binds,
  savepoints and migrations.
  - `sqlez_macros::sql!` formats SQL and **checks its syntax at compile time** against an
    in-memory SQLite (`crates/sqlez_macros/src/sqlez_macros.rs:4-31`; macOS and Windows only).
- **Opening a file:** `ThreadSafeConnection::builder::<M: Migrator>(path, persistent=true)`
  (`thread_safe_connection.rs:143-155`), then:
  - `.with_db_initialization_query(..)`, run once per file, on the write thread (`:62-65`).
  - `.with_connection_initialize_query(..)`, run on every per-thread connection; it must not write
    (`:52-58`).
  - `.build().await`, which runs migrations in a savepoint and retries up to 10 times
    (`:79-127`).
- **Domains and migrations.**
  - `trait Domain { const NAME; const MIGRATIONS: &[&str]; fn should_allow_migration_change(..) }`
    (`domain.rs:3-10`). Tuples of up to 5 domains form a `Migrator` (`:32-64`).
  - Migrations are stored in a `migrations(domain, step, migration)` table. A stored step whose
    text no longer matches (after `sqlformat`) **fails the open** (`migrations.rs:37-110`, bail at
    `:87-95`). So migrations are append-only, never edited.
- **Threading.**
  - Reads: `ThreadSafeConnection` derefs to a **per-thread** `Connection`, created lazily, with
    writes disabled (`:238-240`, `:272-280`). Reads are synchronous on the calling thread.
  - Writes: `conn.write(|c| ..)` queues the closure to one background thread per file,
    `sqlezWorker`, and returns a future (`:169-190`, `:282-304`). Tests use `locking_queue()`
    (`:306-314`).
  - So do reads inside `cx.background_spawn` and await writes.
- **`db`** (`crates/db`) builds Zed's `AppDatabase` on top.
  - The file is `database_dir()/0-<channel>/db.sqlite` (`db.rs:164-168`), with pragmas WAL,
    `busy_timeout=500`, `synchronous=NORMAL`, `case_sensitive_like` and `foreign_keys` (`:125-134`).
  - Domains register through `db::static_connection!` and `inventory` (`:246-285`).
  - On failure it falls back to an in-memory DB and only sets a flag (`:174-225`).
  - The spike must already create `AppDatabase`, because `workspace` and `editor` need it
    (`research/zed-crates.md` §1).

### `rusqlite` or `sqlez`?

- **Zed pins `libsqlite3-sys = { version = "0.30.1", features = ["bundled"] }`** (`Cargo.toml:684`).
  That bundles **SQLite 3.46.0** (`libsqlite3-sys-0.30.1/sqlite3/sqlite3.h:149`). `sqlez` depends
  on it directly.
- **`libsqlite3-sys` declares `links = "sqlite3"`** (its `Cargo.toml:18`). Cargo allows only one
  package with a given `links` value in a graph. So `rusqlite` works only at a version that also
  takes `libsqlite3-sys` 0.30.x.

  | rusqlite | requires libsqlite3-sys | Fits Zed? |
  |---|---|---|
  | 0.32.1 (2024-07-26) | `^0.30.1` | yes |
  | 0.33.0 (2025-01-19) | `^0.31.0` | no: `links` conflict |
  | … 0.41.1 (2026-10-28, latest) | `^0.39.0` | no |

  (crates.io dependency API.) Bumping `libsqlite3-sys` in rozed's lock isn't possible either: `sqlez`
  asks for `0.30.1`, and 0.31 is semver-incompatible.
- **Recommendation: use `sqlez`.** It's already compiled in. It gives compile-time SQL syntax
  checks, the same migration model as the rest of the app, and a write thread that keeps the GPUI
  threads off disk writes. `rusqlite` 0.32 would mean a two-year-old API pinned until Zed bumps
  SQLite, for no gain.

### How rozed should use it

- **Its own file, its own domain,** not `AppDatabase`.
  - Path: `paths::database_dir()/0-<channel>/rozed.sqlite`, next to Zed's file. Dev and release
    stay apart, which matters because their tokens live in different stores (section 2).
  - Open with `ThreadSafeConnection::builder::<RozedDb>(path, true)` and Zed's pragmas.
- **Don't open through `db::open_db`.** It silently falls back to memory. rozed's file holds
  durable state (accounts per owner, "last seen", trusted repos), and losing it silently is worse
  than an error.
- **Watch the in-memory fallback in `sqlez` too.** `Connection::open_file` falls back to an
  in-memory DB if `sqlite3_open_v2` fails (`connection.rs:64-66`), and that applies to every
  per-thread connection.
  - `create_dir_all` the directory first.
  - After `build()`, check `PRAGMA database_list` reports the real path, and fail loudly if not.
- **Tables** (`STRICT`, as Zed does). The cache part is the only part "Clear cache" may delete
  (design §3).
  - Durable:
    - `accounts(host, login, user_id, source, scopes, added_at)`
    - `owner_accounts(host, owner, login)`
    - `recent_prs(host, owner, repo, number, login, opened_at)`
    - `last_seen(host, owner, repo, number, head_oid, seen_at)`
    - `trusted_repos(..)` and `trusted_prs(..)`
  - Cache:
    - `http_cache(account, url, accept, etag, last_modified, status, headers_json, body BLOB, fetched_at)`
    - `pr_cache(host, owner, repo, number, account, operation, data_json BLOB, fetched_at)`
- **Tests:** an in-memory or temp-file DB opened with `locking_queue()` (as `db::open_test_db`
  does, `db.rs:227-239`) and a unique name per test.

---

## 4. Typed GraphQL

### GitHub's schema: where and how big

| Source | Size | Fresh? | Notes |
|---|---|---|---|
| `https://docs.github.com/public/fpt/schema.docs.graphql` | 1,562,841 bytes, 75,244 lines | `last-modified: 2026-10-30`, `cache-control: max-age=604800` | The documented download ([Public schema](https://docs.github.com/en/graphql/overview/public-schema)). The old `/public/schema.docs.graphql` is now **404** **[observed]**. Includes `PullRequestStack`. |
| `github/docs` `src/graphql/data/fpt/schema.docs.graphql` (raw on `main`) | 1,584,905 bytes | Updated daily by "GraphQL schema update" commits (#64579 on 2026-11-05) | Ahead of the CDN copy: it already had new `CopilotCodeReview*` types **[observed]**. Pin by commit SHA for reproducibility. |
| `octokit/graphql-schema` `schema.graphql` | 1,223,842 bytes | Last schema commit **2025-02-27** | **Stale**: no `PullRequestStack`. Don't use it. |
| Live introspection | — | Exact | Needs a token. Gives JSON, not SDL. Fine for a one-off diff, not the vendored copy. |

- **Contents of the docs schema:** 1,032 object types, 792 other definitions, and three directive
  declarations (`@preview`, `@possibleTypes`, `@docsCategory`, the last used 1,213 times).
  - `graphql-parser` (graphql_client), `cynic-parser` and `apollo-compiler` all accept it
    **[observed]**.
  - It gzips to 205 KB.
- **Vendoring.** Commit `crates/github/graphql/schema.graphql` plus
  `script/refresh-github-schema`, which:
  1. downloads `https://raw.githubusercontent.com/github/docs/<sha>/src/graphql/data/fpt/schema.docs.graphql`
     (default: the latest `main` commit, recorded in a header comment), and
  2. runs `cargo test -p github graphql_schema` (validation, below) and `cargo build -p github`.

  A schema change that breaks a query then fails the PR that refreshes it, as design §12 wants. CI
  needs no network.

### Experiment (Rust 1.98.1, debug, M-series, 18 cores)

- **Setup.** One `.graphql` file with two operations:
  - `PollPullRequest`: `rateLimit`, `pullRequest` with `stack`, `stackEntry`, `mergeQueueEntry`,
    `statusCheckRollup` and `timelineItems { updatedAt }`.
  - `RepoOpenPulls`: 100 open PRs with branches and stack fields.
- **Tools.** The same query as `graphql_client` derives, as `cynic` structs (schema crate plus
  query crate, `rkyv` on, as cynic's "Large APIs" guide says), and through `apollo-compiler`
  validation.

| | graphql_client 0.16.0 | cynic 3.14.0 (+rkyv) | apollo-compiler 1.33.1 |
|---|---|---|---|
| Clean build incl. all deps | 4 s | 13 s | 6 s |
| The crate alone, from clean | **0.3 s** | schema crate **4.5 s** + query crate 3.3 s | n/a (runtime: parse + validate schema 0.26 s) |
| Field typo (`updatdAt`) | **compile error** "No field named updatdAt on PullRequest" | **compile error**, with "Did you mean `updatedAt`?" | **error** |
| Argument name typo (`pullRequest(numbr:)`) | **compiles** (passed through as text) | **compile error** "no such field" / "missing fields - number" | **error** "required argument `Repository.pullRequest(number:)` is not provided" |
| Variable of the wrong type (`$number: String!` into `Int!`) | only caught because the Rust caller passed an `i32` | type-checked | **error** "variable `$number` of type `String!` cannot be used for argument `number` of type `Int!`" |
| Interface or union without `__typename` | compile error (`query/validation.rs`) | handled | — |
| New crates vs Zed's lock (normal + build deps) | 5: `graphql_client`, `graphql_query_derive`, `graphql_client_codegen`, `graphql-parser` 0.4, `graphql-introspection-query` (`combine` and `heck` already in Zed) | ~19, incl. the `rkyv` 0.8 family (rkyv, bytecheck, munge, ptr_meta, rancor, rend) and **duplicate majors** of `logos` 0.14 (Zed has 0.15/0.16), `lalrpop-util` 0.22 (Zed 0.23) and `strsim` 0.10 (Zed 0.11) | ~12, dev-only (`apollo-parser`, `rowan`, `ariadne`, `serde_json_bytes`, …) |
| Generated code | Only the types the query selects, in a module per operation (`poll_pull_request::PollPullRequestRepositoryPullRequest`, `poll_pull_request::PullRequestState`) **[observed]** | Marker types for the **whole** schema in the schema crate, plus your own structs | — |
| Runtime deps | `serde`, `serde_json` (its `reqwest` feature is optional; leave it off) | `serde`, `ref-cast`, `static_assertions`, `thiserror` 1 (its `reqwest ^0.13` feature is optional) | — |
| Maintenance | `graphql-rust/graphql-client` on GitHub. Releases 0.15.0 (2025-12-08) and 0.16.0 (2026-01-15). Commits through 2026-08-11. 112 open issues. | **GitHub repo archived 2026-03-07** ("Add migration notice"). Development moved to `codeberg.org/obmarg/cynic` (commits to 2026-10-14, MSRV 1.95). Releases 3.13 (2026-02) and 3.14 (2026-07). | Apollo, active (1.33.1 on 2026-10-21) |

- **Neither crate pulls an HTTP client.** Both build a serde body that rozed sends through
  `HttpClient`.
- **Zed's workspace `serde_json` enables `preserve_order` and `raw_value`** (`Cargo.toml:894`).
  - Feature unification turns them on for rozed's crates too. That's harmless for decoding.
  - It matters for fixture keys: `serde_json::Value` keeps insertion order, so canonicalise by
    sorting keys yourself (section 8).

### Recommendation: `graphql_client` + `apollo-compiler` validation

- **Queries are plain `.graphql` files** (`crates/github/graphql/*.graphql`).
  - They're readable, can be pasted into GitHub's explorer or sent with `gh api graphql`, and the
    fixture recorder reuses the exact text.
  - Fragments can be shared between operations in the same file.
- **The cheapest compile:** only selected types are generated. The 1.5 MB schema is parsed once
  per crate build, cached across derives (`SCHEMA_CACHE`, `graphql_client_codegen/src/lib.rs:47`).
- **Close its gap with one unit test.** `graphql_schema_valid` reads the vendored schema with
  `apollo_compiler::Schema::parse_and_validate`, then every `.graphql` file with
  `ExecutableDocument::parse_and_validate`. That's full spec validation: arguments, variable types
  and fragment spreads. `apollo-compiler` is a dev-dependency only.
- **Deprecations:** keep graphql_client's default `deprecated = "warn"`. Generated fields get
  `#[deprecated]`, and CI's `clippy -D warnings` then fails on any use of a field GitHub deprecates
  after a schema refresh **[inferred]**.
- **Custom scalars** must be in scope under their schema names: `DateTime`, `GitObjectID`, `URI`,
  `HTML`, `GitTimestamp`, `BigInt`, `Base64String` and so on. For example
  `type DateTime = String;`, or `chrono::DateTime<Utc>`, which Zed already has.
- **Decode the response with rozed's own envelope,** not `graphql_client::Response`. GitHub puts a
  top-level `type` on each error, and graphql_client's `Error` has only `message`, `locations`,
  `path` and `extensions` (`graphql_client/src/lib.rs:197-206`). **[observed]** examples:
  - Data-level: `{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","path":["repository"],..,"message":"Could not resolve to a Repository with the name 'rozed-dev/does-not-exist-xyz'."}]}`
    with HTTP 200.
  - Validation: `{"errors":[{"path":[..],"extensions":{"code":"undefinedField",..},"message":"Field 'nope' doesn't exist on type 'PullRequest'"}]}`.
  - Bad token: HTTP **401** for both REST and GraphQL.

  Decode as `struct GqlResponse<T> { data: Option<T>, errors: Vec<GqlError> }` with
  `GqlError { r#type: Option<String>, message, path, extensions }`. Then handle partial data
  (`data` and `errors` both set) explicitly.
- **Cynic is the alternative** if you prefer queries as Rust structs checked in one tool. Its costs:
  a heavier dependency set with duplicate major versions, a schema crate that recompiles its 4.5 s
  on every schema refresh, and a project that just moved off GitHub.
- **Ruled out.**
  - `octocrab` (in Zed's lock only for `tooling/compliance`): untyped GraphQL and its own HTTP
    stack.
  - The `graphql-client` CLI (committing generated Rust): the derive is already fast.

---

## 5. Device flow

### Endpoints and observed behaviour

| Step | Request | Response **[observed 2026-11-06]** |
|---|---|---|
| Codes | `POST https://github.com/login/device/code`, `Accept: application/json`, form `client_id=Ov23liBORqwRdiPzqsPP&scope=repo read:org workflow` | `200` `{"device_code":"<40 chars>","user_code":"50B0-5666","verification_uri":"https://github.com/login/device","expires_in":899,"interval":5}` |
| Poll | `POST https://github.com/login/oauth/access_token`, `Accept: application/json`, form `client_id`, `device_code`, `grant_type=urn:ietf:params:oauth:grant-type:device_code` | **All outcomes are HTTP 200**; check `error` |
| Pending | (poll after ≥5 s) | `{"error":"authorization_pending","error_description":"The authorization request is still pending.","error_uri":"https://docs.github.com/developers/apps/authorizing-oauth-apps#error-codes-for-the-device-flow"}` |
| Too fast | (immediate re-poll) | `{"error":"slow_down","error_description":"Too many requests have been made in the same timeframe.","error_uri":"https://docs.github.com","interval":10}`, then `"interval":15`, then `20`. **Cumulative**, and each response carries the new interval |
| Bad device code | | `{"error":"incorrect_device_code","error_description":"The device_code provided is not valid.",..}` |
| No `grant_type` | | `{"error":"incorrect_client_credentials",..}`. The docs say `unsupported_grant_type`; GitHub actually answers this |
| Unknown client ID | (`/login/device/code`) | **`404`** `{"error":"Not Found"}` |
| No `Accept` header | | `200`, `content-type: application/x-www-form-urlencoded`, `error=slow_down&…&interval=20`. **Always send `Accept: application/json`** |
| Expired code | (poll ~17 min after the code was issued) | `{"error":"expired_token","error_description":"This 'device_code' has expired.",..}`. The docs' table also says `token_expired`; the real code is **`expired_token`** |
| Success | | `{"access_token":"gho_…","token_type":"bearer","scope":"read:org,repo,workflow"}` (`research/github-checks.md` §2) |

Docs: [Authorizing OAuth apps, Device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow).

- **Rules.**
  - `interval` is the minimum gap between polls.
  - `slow_down` adds 5 s to it.
  - The codes expire after `expires_in` (900 s; 899 observed).
  - `access_denied` means the user pressed Cancel. The code can't be reused.
  - `device_flow_disabled` means the app setting is off.
  - "50 submissions in an hour per application" of user codes.
- **Token limits** (`data/reusables/apps/oauth-token-limit.md`).
  - At most **10 tokens per user/app/scope**, and **10 created per hour**.
  - An 11th token revokes the oldest never-used token, else the least recently used one.
  - Hitting the hourly limit triggers a re-authorization prompt instead.
  - **For testing:** repeated device-flow sign-ins as `rozed-bot` with the same scopes will
    eventually revoke `~/.config/rozed-dev/oauth-token`. Re-signing in on every test run would
    churn it.
- **Expiring tokens.** If the app ever opts into expiring tokens (or the request adds
  `offline_access`), the response adds `refresh_token`, `expires_in` (28800) and
  `refresh_token_expires_in`. rozed's app doesn't, so tokens are long-lived.

### Recommended loop

1. **Request the codes.** Show `user_code` in large type with a Copy button (copy it automatically
   too). `cx.open_url("https://github.com/login/device")`. Show a countdown to `expires_in`.
2. **Poll** with `interval × 1.2` and a floor of 5 s. gh uses ×1.2 and switches to ×1.4 after a
   `slow_down`, against clock drift (`cli/oauth device/device_flow.go:150-283`). Use GPUI's
   `background_executor().timer(..)`, so tests can advance a fake clock.
   - `authorization_pending`: continue.
   - `slow_down`: set the interval to the response's `interval` (else +5 s) and continue. On a
     second `slow_down`, keep going, unlike gh, which bails: rozed runs no VMs with drifting clocks.
   - `expired_token`, `access_denied`: stop, and offer "Start again".
   - Anything else (`incorrect_*`, `unsupported_grant_type`, `device_flow_disabled`, a non-JSON
     body, non-200): stop with the error text. These are bugs or misconfiguration.
   - Stop polling at the deadline without waiting for GitHub to say so.
3. **On a token:** `GET https://api.github.com/user`.
   - Read `login` and `id`.
   - Check that `x-oauth-scopes` contains `repo`, `read:org` and `workflow`. **[observed]** The
     header is `read:org, repo, workflow` (`research/github-checks.md` §2).
   - Store the token under `rozed://github.com/<login>`, replacing any token for that login.
4. **Cancel** drops the polling task. Dropping the future aborts the in-flight request
   (section 1).

### Blocked by an org (design §3)

- **What happens.** With OAuth app access restrictions on (the default for new orgs), an unapproved
  OAuth app's token keeps public access but gets **no API access to the org's private resources,
  and no create, update or delete even on its public ones**
  ([About OAuth app access restrictions](https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions)).
- **What the API returns.**
  - The REST error message is: "Although you appear to have the correct authorization
    credentials, the `<org>` organization has enabled OAuth App access restrictions, meaning that
    data access to third-parties is limited. For more information on these restrictions, including
    how to enable this app, visit https://docs.github.com/articles/restricting-access-to-your-organization-s-data/"
    (quoted in [pages-cms#57](https://github.com/pages-cms/pages-cms/issues/57)).
  - **[inferred]** Status `403`, and in GraphQL an error with `type: "FORBIDDEN"` carrying the same
    message.
  - Private repos may simply read as `404` / `NOT_FOUND` ("App owners cannot see whether their app
    is blocked",
    [Troubleshooting the REST API](https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api)).
- **Detect** a 403 or GraphQL error whose message contains "OAuth App access restrictions".
  - Show: "`<org>` hasn't approved rozed. Request approval on GitHub", linking to
    **`https://github.com/settings/connections/applications/Ov23liBORqwRdiPzqsPP`**.
  - That page lists each org with a **Request** button
    ([Requesting organization approval](https://docs.github.com/en/account-and-profile/how-tos/organization-membership/requesting-organization-approval-for-oauth-apps);
    URL form from "Directing users to review their access" on the OAuth page).
  - Members can request; outside collaborators only if the org allows it.
  - **[observed]** The device-flow consent page already shows each restricted org with
    Grant/Request (`research/github-checks.md` §2).
- **On a `NOT_FOUND`** for a PR link in an org repo, say "Not found, or `<org>` restricts rozed",
  with the same link. rozed can't tell the two apart.

### SAML SSO (design §11)

- **The header.** A token that isn't authorized for an SSO org gets `404` or `403`. On `403`,
  **`X-GitHub-SSO: required; url=<url>`** carries a link that authorizes it, valid for one hour.
- **Lists** across orgs return partial results with
  `X-GitHub-SSO: partial-results; organizations=21955855,20582480`
  ([Authenticating to the REST API](https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api)).
- **OAuth tokens** are SSO-authorized when they're created, but only for orgs where the user has
  an active SAML session at that moment
  ([Authorizing OAuth apps](https://docs.github.com/en/apps/oauth-apps/using-oauth-apps/authorizing-oauth-apps)).
  A user who signs in to rozed without a fresh SAML session gets an unauthorized token for that org.
- **gh's handling.** gh captures `X-GitHub-SSO` from **every** response, GraphQL included, and
  extracts `url=([^;]+)` (`pkg/cmd/factory/default.go:26-27, 215, 348-358`). It prints
  "Authorize in your web browser: <url>" (`pkg/cmd/api/api.go:561-562`).
  - GraphQL's message is "Resource protected by organization SAML enforcement. You must grant
    your … token access to this organization." ([cli/cli#2661](https://github.com/cli/cli/issues/2661)).
- **rozed:** record the header on every response, per account. On a 403 or GraphQL `FORBIDDEN`
  with it, show "Single sign-on required for this org" with the URL. If the URL has expired or is
  missing, offer "Sign in again", the device flow after visiting the org's SSO page.

---

## 6. Importing `gh` accounts

### How `gh` stores accounts (cli/cli v2.103.3)

- **Config dir:** `$GH_CONFIG_DIR`, else `$XDG_CONFIG_HOME/gh`, else `~/.config/gh`
  (`go-gh pkg/config/config.go:273-285`). Accounts are in **`hosts.yml`**. The multi-account
  schema dates from v2.40 (`internal/config/migration/multi_account.go`):

  ```yaml
  github.com:
    user: scornz              # the active account
    git_protocol: https
    oauth_token: gho_…        # only with --insecure-storage; mirrors the active user's
    users:
      scornz:                 # one key per account; empty when the token is in the keyring
      other-login:
        oauth_token: gho_…    # only with --insecure-storage
  ```

- **Keyring.** `zalando/go-keyring`, which on macOS shells out to `/usr/bin/security`
  `add-generic-password` / `find-generic-password`, and stores the value as
  `go-keyring-base64:<b64>`.
  - **Service `gh:<host>`, account `<login>`** for each user (`config.go:725-727`, `:693-712`).
  - Plus an unkeyed copy of the active user's token (account `""`, `:382-384`).
  - In v2.103.3, expiring ("refreshable") credentials live under **service
    `gh:_refreshable:<host>`** as JSON with `access_token`, `refresh_token` and expiry times
    (`refreshable.go:301-322, 523-528, 601-604`). They're opt-in (`gh auth login --expiring` or
    `expiring_credentials: enabled`).
- **Reading a token.** `gh auth token --hostname <host> --user <login>` prints it to stdout
  (`pkg/cmd/auth/token/token.go`).
  - It looks in the per-user keyring, then the refreshable slot, then `users.<login>.oauth_token`
    (`config.go:693-712`).
  - It refreshes an expiring token first, unless `--no-refresh`.
  - On stderr it warns "This token will expire on …" for refreshable ones.
- **Environment overrides.** `GH_TOKEN`, then `GITHUB_TOKEN` (github.com, ghe.com and localhost),
  and `GH_ENTERPRISE_TOKEN` / `GITHUB_ENTERPRISE_TOKEN` (other hosts) win over everything for
  the **active-account** path (`go-gh pkg/auth/auth.go:58-98`; `config.go:295-327`).
  - **With `--user`, gh reads that user's stored token and ignores the env vars**
    (`TokenForUser`, `:693-712`).
  - `gh auth status` reports an env token as an extra active entry with
    `tokenSource: "GH_TOKEN"`.
- **Listing without tokens.** `gh auth status --json hosts` (since ~v2.80, 2025-09) prints, per
  host, entries with:
  - `state`, `active`, `host`, `login`, `tokenSource` (`keyring`, `oauth_token`, `GH_TOKEN`, …)
  - `scopes`, `gitProtocol`, `refreshable`, `tokenExpiresAt`

  The token is **dropped** unless `--show-token` (`pkg/cmd/auth/status/status.go:33-56, 363-370`).
  It checks each token against the API, and always exits 0 with `--json`.
- **Scopes.** gh's own OAuth app (client `178c6fc778ccc68e1d6a`) asks for **`repo read:org gist`**
  by default (`internal/authflow/flow.go:23, 45`). There's **no `workflow`** unless the user added
  it (`gh auth refresh -s workflow`).

### Cleanest import

1. **Find `gh`.** In order: `$GH_PATH`, `/opt/homebrew/bin/gh`, `/usr/local/bin/gh`,
   `~/.local/bin/gh`, then `$SHELL -lc 'command -v gh'`.
   - **[inferred]** An app started from Finder or Dock gets launchd's minimal `PATH`, so a bare `gh`
     lookup fails.
   - On this devbox `~/.local/bin/gh` is a Coder wrapper whose token is a GitHub App `ghu_` token
     (`research/github-api.md` §2). Importing it gives a token that can't upload attachments. Show
     the token type from its prefix.
2. **Run** `gh auth status --json hosts --hostname github.com`, with `GH_TOKEN`, `GITHUB_TOKEN`,
   `GH_ENTERPRISE_TOKEN` and `GITHUB_ENTERPRISE_TOKEN` **removed** from the child's environment.
   Also set `GH_PROMPT_DISABLED=1`, `GH_NO_UPDATE_NOTIFIER=1` and `NO_COLOR=1`; keep
   `GH_CONFIG_DIR`. List each `login` with its scopes, a "short-lived" badge if `refreshable`, and
   a warning if `workflow` is missing.
3. **For each account the user picks,** run
   `gh auth token --hostname github.com --user <login>` with the same environment.
   - Read stdout through a pipe into a secret buffer, trimmed. Never log it.
   - Discard stderr except to show a non-zero exit's message.
   - Verify with `GET /user`: its `login` must equal `<login>`, and record `x-oauth-scopes`.
   - Store the token like a device-flow token, with `source = gh`.
4. **Don't read gh's Keychain items directly.**
   - **[inferred]** They were created by `/usr/bin/security`, so rozed's own Security-framework
     reads would trigger an access prompt per item.
   - Their format (base64 prefix, the refreshable JSON) is gh's private detail.
5. **Refreshable gh accounts.** rozed's copy expires within 8 hours, and rozed must **never** use
   the refresh token: that would rotate gh's credential under it. On a `401` for a `source = gh`
   account, re-run step 3 once (gh refreshes and saves). If that fails, mark the account signed out
   (design §11). The same retry also covers a user who re-logged in `gh`.

---

## 7. Polling cheaply

### Facts

| Fact | Evidence |
|---|---|
| 304s are free | "Making a conditional request does not count against your primary rate limit if a `304` response is returned and the request was made while correctly authorized with an `Authorization` header" ([Best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#use-conditional-requests)). **[observed]** on every endpoint below, `x-ratelimit-used` didn't move on the 304 and went up by 1 on the next plain request. |
| Which REST endpoints give ETags | **[observed]**, all `200` with an `etag`, all `304` on `If-None-Match`: `GET /repos/{o}/{r}`, `/pulls/{n}`, `/pulls?state=open`, `/pulls/{n}/reviews`, `/pulls/{n}/comments`, `/pulls/{n}/commits`, `/pulls/{n}/files`, `/issues/{n}/comments`, `/issues/{n}/timeline`, `/issues/{n}/events`, `/commits/{sha}/check-runs`, `/commits/{sha}/status`, `/commits/{sha}/check-suites`, `/stacks?pull_request=n`, `/stacks/{n}`, `/compare/a...b`, `/notifications` (also sends `x-poll-interval: 60`), `/user`. `last-modified` was also present on pulls/{n}, commits, files, compare, repo and user. |
| No ETag | `GET /rate_limit`, which doesn't count against the primary limit. `GET /rules/branches/main` on the private free-plan repo returned `403` "Upgrade to GitHub Pro or make this repository public to enable this feature." **[observed]** So branch rules need a public or paid repo, which matters for the e2e repo. |
| ETags are per token and per `Accept` | **[observed]** The same URL gave a different ETag for the bot's PAT than for rozed's OAuth token, and replaying one token's ETag with the other got a `200`. Changing `Accept` also changed it. `Vary: Accept-Encoding, Accept, X-Requested-With`. Key the cache by (account, URL, `Accept`, API version). |
| GraphQL has no conditional requests | **[observed]** No `etag` on `POST /graphql`. The minimum cost is **1 point** per call: the `PollPullRequest` query and the 100-PR `RepoOpenPulls` query each cost `1` (`rateLimit.cost`). |
| GraphQL headers | `x-ratelimit-limit: 5000`, `-remaining`, `-used`, `-reset`, `x-ratelimit-resource: graphql`. "When possible, you should use the rate limit response headers instead of querying the API" ([GraphQL rate limits](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)), so `rateLimit { cost }` is only for development. |
| Over the GraphQL primary limit | Status still **200**, an error message, and `x-ratelimit-remaining: 0`. |
| Over the REST primary limit | `403` or `429`, `x-ratelimit-remaining: 0`, wait until `x-ratelimit-reset` ([REST rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)). |
| Header values can disagree | "rate limit values can vary from one response to the next based on the location handling the request". **[observed]** `used` jumped from 23 to 1 and back to 25 between consecutive requests. Treat the counts as approximate: keep the lowest `remaining` seen per reset window. |
| The budget is shared | 5,000 REST requests/h and 5,000 GraphQL points/h **per user**, "includes requests made with a PAT as well as requests made by a GitHub App or OAuth app on behalf of a user". rozed shares it with `gh`, agents and everything else the user runs. (15,000 / 10,000 if the app is approved by a GHEC org.) |
| Secondary limits | No more than 100 concurrent requests (REST and GraphQL combined). Per minute: 900 points for REST endpoints and 2,000 for GraphQL. 90 s of CPU per 60 s. 80 content-creating requests/min and 500/h. GET = 1 point, mutating = 5. "Make requests serially." Wait ≥1 s between mutations. On a secondary limit (`403`/`429`, or GraphQL `200` or `403`), honour `retry-after`, else `x-ratelimit-reset` if remaining is 0, else wait ≥60 s with exponential backoff. There's no way to query secondary status. **[inferred]** 304s probably still count toward secondary limits. |
| Timeouts cost extra | A GraphQL request over 10 s is killed, and "additional points will be deducted from your primary rate limit for the next hour". Keep queries small. |
| Use stable list order | `sort=updated` reorders pages and defeats 304s. Use the default order and the same parameters every time. |
| `updatedAt` misses some changes | **[observed]** On PR #1, `PullRequest.updatedAt` was `08:15:07Z`, but `timelineItems.updatedAt` was `08:15:21Z`, the stack-add event. **Adding a PR to a stack didn't bump the PR's `updatedAt`.** Fingerprint with both, plus the head OID. |
| No REST merge queue | No REST path contains "queue" in GitHub's OpenAPI description (2026-11-06). The queue is only in GraphQL: `Repository.mergeQueue(branch:)`, with `entries`, `nextEntryEstimatedTimeToMerge` and `configuration`, and `PullRequest.mergeQueueEntry` with `position`, `state`, `enqueuedAt`, `estimatedTimeToMerge`, `solo` and `jump`. |

### Recommended scheme (design §3)

- **Per-account scheduler.**
  - One queue per account, so requests run serially, with at most 2 in flight. Mutations are
    spaced ≥1 s apart.
  - Every response updates that account's `RateLimitState`: `core` and `graphql` resources,
    lowest remaining, reset time, secondary-limit cooldown, and the last SSO header.
- **Tier A, the open PR** in the active window, every 20 s (the 15–30 s range, adjustable):
  1. `GET /repos/{o}/{r}/pulls/{n}` with `If-None-Match`.
     - It's free when unchanged. It catches a push (`head.sha`), a base change, state, draft,
       title or body edits, `stack`, and mergeability once computed.
  2. A 1-point GraphQL fingerprint, every tick or every third tick:

     ```graphql
     query PrFingerprint($owner: String!, $name: String!, $number: Int!) {
       repository(owner: $owner, name: $name) {
         pullRequest(number: $number) {
           updatedAt headRefOid mergeStateStatus reviewDecision
           timelineItems { updatedAt totalCount }
           reviewThreads(first: 100) { totalCount nodes { isResolved comments { totalCount } } }
           commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
           mergeQueueEntry { state position }
         }
       }
     }
     ```

     Its cost is about 1–2 points by GitHub's formula **[inferred]**. Check `rateLimit.cost` in
     the fixture recorder.
  3. Only when (1) returns `200` or (2)'s hash changes, fetch details for what changed:
     - the timeline after the stored cursor
     - threads
     - files when the head moved
     - checks via REST `check-runs` / `status` with ETags, while any check is pending
- **Tier B, merge queue,** while the PR is queued or the queue view is open: every 20 s,
  `mergeQueue(branch:) { entries(first: 100) { totalCount nodes { id position state enqueuedAt
  estimatedTimeToMerge solo jump pullRequest { number } headCommit { oid } } } }` (1 point).
- **Tier C, other tabs and stack siblings,** every 2 min: `GET /repos/{o}/{r}/pulls?state=open&per_page=100`
  with `If-None-Match`.
  - It's free when no open PR changed. It feeds stack detection (branches and head SHAs) and each
    sibling's state.
  - Page with the default sort.
- **Background** (`cx.observe_window_activation` / `window.is_window_active()`,
  `gpui/src/app/context.rs:410`, `gpui/src/window.rs:3018`): every tier every 5 min.
  - On activation, poll Tier A at once.
  - Honour any `x-poll-interval`.
- **Rate-limit governor,** per resource:
  - `remaining < 20%` of limit: double every interval, and show "Polling slowed: GitHub rate limit
    low" (design §3).
  - `< 5%`: only Tier A, every 2 min.
  - `0`, or a secondary cooldown: pause until reset or `retry-after`, with a banner showing the
    time (design §11).
  - Network errors: exponential backoff with jitter, 20 s up to 5 min, and the offline banner.
- **Budget [inferred].**
  - One open PR with an active queue: Tier A GraphQL ≈ 180 points/h, Tier B ≈ 180/h, plus detail
    fetches.
  - REST is close to 0 while nothing changes.
  - Five other tabs ≈ 0–150 REST/h.
  - That's under 10% of the shared GraphQL budget. Run the fingerprint every third tick (60 points/h)
    if the user's other tools are heavy.
- **ETag cache.**
  - Key: (account, method GET, full URL with query, `Accept`, API version). Value: `etag`,
    `last-modified`, status, the headers rozed reads, and the body.
  - Keep it in memory, and write it to the `http_cache` table so a reopened PR shows instantly and
    its first poll can still get a 304.
  - On a 304, serve the stored body.
  - Never send `If-None-Match` with a different token's ETag.

---

## 8. Fixtures: recording and replay

### Replay: `ReplayHttpClient`

```rust
pub struct ReplayHttpClient(Arc<Mutex<Replay>>);   // impl http_client::HttpClient
struct Replay { by_key: HashMap<Key, VecDeque<Recorded>>, seen: Vec<SeenRequest>, strict: bool }
// Key = REST:    (method, path, sorted query pairs)
//       GraphQL: ("POST /graphql", operationName, sha256(canonical variables JSON))
```

- **`send`** reads the request body (`AsyncBody` implements `AsyncRead`), builds the key, and pops
  the next recorded response for that key.
  - When one response remains, it's repeated (steady state for polling).
  - An unknown key panics, listing the key and the keys available.
  - Each `SeenRequest` is kept: method, URL, headers minus `Authorization`, and body. Tests then
    assert that `If-None-Match` was sent, that `Authorization` used the `token` scheme with the fake
    token, and how many calls were made.
- **`Recorded`** is `{status, headers: allowlist, body}`. The response comes back as
  `AsyncBody::from(Vec<u8>)`.
- **Conditional requests replay as recorded:** a cassette for "unchanged" records the real `304`.
- **Canonical variables:** serialize, parse to `serde_json::Value`, then write with keys sorted
  recursively. `preserve_order` is on in Zed's workspace (section 4), so `to_string` alone isn't
  canonical.
- **Stale queries:** each GraphQL entry also stores `query_sha256` of the operation's text. A test
  fails with "re-record cassette X" when the `.graphql` file no longer matches. The key leaves the
  query text out on purpose, so a whitespace edit gives a clear "stale" error, not a "no such key"
  panic.
- **Polling tests** use `#[gpui::test]`: drive the poller with
  `cx.executor().advance_clock(Duration::from_secs(20))` and `run_until_parked()`
  (`gpui/src/executor.rs:183-219`). Then assert the request sequence: free REST first, GraphQL only
  on change.

### Files

```
crates/github/tests/fixtures/
  <cassette>/                       # e.g. pr1_open_unchanged, stack3_layers, queue_empty
    cassette.json                   # metadata: recorded_at, repo, schema sha, recorder version
    001-GET-repos_rozed-dev_playground_pulls_1.json
    002-GET-repos_rozed-dev_playground_pulls_1.json   # the 304
    003-POST-graphql-PollPullRequest.json
```

Each interaction file is `{request: {method, url, operation?, variables?, query_sha256?,
if_none_match?}, response: {status, headers, body}}`. The body is stored as a JSON value
(pretty-printed, so diffs are readable) or base64 for binary. One file per interaction keeps
re-recording diffs small.

### Recording

- **A recorder that runs rozed's real code paths.** `RecordingHttpClient(inner: Arc<dyn HttpClient>, dir)`
  wraps a real `ReqwestClient`, tees each response, and returns it unchanged.
  - Run it from an `#[ignore]` test or `cargo xtask record-fixtures <cassette>`, which calls the
    same `GithubClient` functions the app does.
  - Token: from `ROZED_FIXTURE_TOKEN_FILE`, default `~/.config/rozed-dev/oauth-token` (rozed's
    own `gho_` token as `rozed-bot`; the same token type users have).
  - Fall back to `ROZED_BOT_TOKEN` (`~/.config/rozed-dev/bot.env`) only for calls the OAuth token
    can't make.
  - **Don't run a device flow per recording** (the 10-token limit in section 5).
- **Target:** `rozed-dev/playground`, private.
  - PR #1 (`stack-a` → `main`) and PR #2 (`stack-b` → `stack-a`) form stack #3. The repo id is
    `1447997428` and `main` is at `6ce218eb`.
  - Cassettes that need state changes (a new push, a review, a queue) are recorded as scripted
    sequences: change state with the bot's token, then record.
  - Merge queues and branch rules need a public or paid org repo: the `/rules` `403` above, and
    design §12. Those cassettes wait for `rozed-dev/e2e` (status.md "Waiting on the user").
- **Keeping tokens out.**
  1. Never store request headers. Store the method, the URL, and the GraphQL body's operation name
     and variables only.
  2. Response headers from an allowlist only: `etag`, `last-modified`, `link`, `location`,
     `content-type`, `retry-after`, `x-poll-interval`, `x-github-sso`, `x-oauth-scopes`,
     `x-accepted-oauth-scopes`, `x-ratelimit-*` and `x-github-api-version-selected`.
     Drop `x-github-request-id`, `set-cookie` and the rest.
  3. Rewrite signed S3 URLs (`X-Amz-Signature`, `X-Amz-Credential`,
     `X-Amz-Security-Token`) to `REDACTED`.
  4. **Hard check:** after writing, scan every file for the token's bytes and for
     `gh[opsur]_[A-Za-z0-9]{36,}` and `github_pat_[A-Za-z0-9_]{50,}`. Delete the cassette and fail
     if any match.
  5. The repo is private but rozed's repo is public. Committing fixtures publishes the
     playground's contents, node IDs and the bot's commit email (`rozed-test@scornz.com`). Keep
     the playground free of anything sensitive.
- **Synthetic cassettes** cover what can't be recorded on demand:
  - device flow (shapes from section 5, with `device_code`/`access_token` set to obviously fake
    values such as `gho_FAKEFAKE…`)
  - rate limit exhausted (headers edited)
  - secondary limits with `retry-after`
  - `X-GitHub-SSO: required; url=…`
  - the OAuth-restriction 403
  - `401 Bad credentials`

  Mark them `"synthetic": true` in `cassette.json` and keep them small.

---

## Open questions for implementation

- **Which actions change `GET /pulls/{n}`'s ETag** (a review, a thread resolve, a check
  completing, a stack change), and **what the fingerprint query really costs.** Measure both while
  recording the first cassettes; tune Tier A from the results.
- **The exact status and GraphQL error `type`** for OAuth-restriction and SSO failures. Both are
  inferred above. Capture them the first time a restricted org is available. `rozed-dev` approved
  rozed, so it can't show this.
- **Whether GitHub accepts `Authorization: Bearer` for uploads.** Unknown; use `token` everywhere
  (`research/github-api.md` §1).
- **Keychain item access for a signed release build:** check that a Developer ID-signed rozed reads
  its own items without prompting across updates. Needs the user's Apple account.

## Sources

- Zed `v1.27.0`:
  - `crates/http_client/src/{http_client.rs,async_body.rs,github.rs}`
  - `crates/reqwest_client/src/reqwest_client.rs`
  - `crates/credentials_provider/src/*`
  - `crates/zed_credentials_provider/src/zed_credentials_provider.rs`
  - `crates/gpui_apple/src/keychain.rs`
  - `crates/gpui_macos/src/platform.rs`
  - `crates/gpui/src/{app.rs,executor.rs,platform.rs,app/context.rs,window.rs,platform/test/platform.rs}`
  - `crates/sqlez/src/*`, `crates/sqlez_macros/src/sqlez_macros.rs`
  - `crates/db/src/{db.rs,kvp.rs,query.rs}`
  - `crates/paths/src/paths.rs`
  - `crates/zed/src/main.rs`
  - `crates/git_hosting_providers/src/providers/github.rs`
  - `Cargo.toml`, `Cargo.lock`
- `zed-industries/reqwest@33bc764`: `src/redirect.rs`, `src/async_impl/client.rs`,
  `src/async_impl/request.rs`.
- crates.io API (2026-11-06): versions and dependencies of `graphql_client`,
  `graphql_client_codegen`, `graphql_query_derive`, `cynic`, `cynic-codegen`, `cynic-parser`,
  `cynic-proc-macros`, `rusqlite` 0.32–0.41, `libsqlite3-sys` and `apollo-compiler`.
- graphql_client: <https://github.com/graphql-rust/graphql-client> (README;
  `graphql_client_codegen/src/{lib.rs,query.rs,query/validation.rs}`; `graphql_client/src/lib.rs`).
- cynic: <https://github.com/obmarg/cynic> (archived), <https://codeberg.org/obmarg/cynic>; book
  `cynic-book/src/large-apis.md`, `manual-http-requests.md`.
- GitHub schema: <https://docs.github.com/public/fpt/schema.docs.graphql>,
  <https://docs.github.com/en/graphql/overview/public-schema>, `github/docs`
  `src/graphql/data/fpt/schema.docs.graphql`, <https://github.com/octokit/graphql-schema>.
- REST OpenAPI: <https://github.com/github/rest-api-description> `descriptions/api.github.com/api.github.com.json`.
- GitHub docs, `github/docs` `main`:
  - [Authorizing OAuth apps](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps)
    (device flow, error codes, token limit reusable)
  - [About OAuth app access restrictions](https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions)
  - [Requesting organization approval for OAuth apps](https://docs.github.com/en/account-and-profile/how-tos/organization-membership/requesting-organization-approval-for-oauth-apps)
  - [Authorizing OAuth apps (user side, SAML)](https://docs.github.com/en/apps/oauth-apps/using-oauth-apps/authorizing-oauth-apps)
  - [Authenticating to the REST API](https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api)
    (`X-GitHub-SSO`)
  - [Troubleshooting the REST API](https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api)
  - [Best practices for using the REST API](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)
  - [Rate limits for the REST API](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)
  - [Rate limits and query limits for the GraphQL API](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)
- cli/cli v2.103.3:
  - `internal/config/config.go`, `internal/config/refreshable.go`,
    `internal/config/migration/multi_account.go`
  - `internal/keyring/keyring.go`, `internal/authflow/flow.go`
  - `pkg/cmd/auth/token/token.go`, `pkg/cmd/auth/status/status.go`
  - `pkg/cmd/factory/default.go`, `pkg/cmd/api/api.go`, `api/http_client.go`
- Other code: cli/go-gh `pkg/auth/auth.go`, `pkg/config/config.go`; cli/oauth
  `device/device_flow.go`; zalando/go-keyring `keyring_darwin.go`.
- Issues: [pages-cms#57](https://github.com/pages-cms/pages-cms/issues/57) (the
  OAuth-restriction message), [cli/cli#2661](https://github.com/cli/cli/issues/2661) (the SAML
  GraphQL error).
