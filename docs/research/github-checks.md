# Live GitHub checks for Lo

Run 2026-11-06, 08:10 to 08:17 UTC, against `rozed-dev` on github.com. Tokens:

- `$BOT`: rozed-bot's classic PAT (`ROZED_BOT_TOKEN`). Scopes: `delete_repo, read:org, repo, workflow`.
- `$OAUTH`: the token Lo's OAuth app minted through device flow in step 2, at
  `~/.config/rozed-dev/oauth-token`.

Every request sends `Authorization: token <…>`. No token, password or code was printed. The
attachment's signed S3 query string is elided.

## Results

| Check | Result |
| --- | --- |
| Lo's OAuth token uploads an attachment | **Yes.** `201`, with the URL returned |
| Stacks on an org repo need an opt-in or setting | **No.** `POST /stacks` returned `201` on a brand-new private org repo |
| OAuth token scopes | `read:org, repo, workflow`. Client `Ov23liBORqwRdiPzqsPP`, prefix `gho_` |
| Fixtures | Private repo `rozed-dev/playground`. PRs #1 (`stack-a` → `main`) and #2 (`stack-b` → `stack-a`). Stack #3. All open |
| Blocked | Nothing |

## 1. Private test repo

```sh
curl -X POST -H "Authorization: token $BOT" https://api.github.com/orgs/rozed-dev/repos \
  -d '{"name":"playground","private":true,"auto_init":true,"description":"rozed test fixtures"}'
```

- **Before.** `GET /repos/rozed-dev/playground` returned `404`, so the repo didn't exist yet.
- **Create.** `201`: `{"full_name":"rozed-dev/playground","private":true,"visibility":"private","default_branch":"main","id":1447997428,"node_id":"R_kgDOVk6v9A"}`
- **`main`.** It has the initial commit `6ce218eb1930c49db64d5744c76a6739d4525e82`, authored by
  rozed-bot.
- **Bot's permission.** `admin`.

**Conclusion:** `rozed-dev/playground` exists and is private, with `main` initialized.

## 2. Device-flow sign-in as the bot with Lo's OAuth app

**Request a device code:**

```sh
curl -X POST -H "Accept: application/json" https://github.com/login/device/code \
  --data-urlencode client_id=Ov23liBORqwRdiPzqsPP --data-urlencode "scope=repo read:org workflow"
```

- `200`: `{"user_code":"2A6B-A54D","verification_uri":"https://github.com/login/device","expires_in":899,"interval":5}`.
  The device code went straight to a file and wasn't printed.

**Approve in the browser.** I used Chrome's existing "rozed-bot" profile window, which was already
signed in to GitHub as rozed-bot, so no password or two-factor code was needed.

1. In a new tab I opened `github.com/login/device`. The account picker showed "Signed in as
   rozed-bot", and I chose Continue.
2. I entered the code, and the consent page "Authorize rozed" appeared: <!-- rename:keep -->
   - Header: "rozed by Mike Scornavacca wants to access your rozed-bot account" <!-- rename:keep -->
   - Organizations and teams: read-only
   - Repositories: public and private
   - Workflow
   - A warning with the requesting IP and time
   - Badges: "Not owned or operated by GitHub", "Created less than a day ago", "Fewer than 10 GitHub
     users"
   - The approve button reads **"Authorize scornz"**, the app owner's login
3. **Organization access.** `rozed-dev` was listed with a **Grant** button, which means the org
   restricts OAuth apps. That's GitHub's default for new orgs. The bot is an org owner, so I clicked
   Grant and the button changed to "Revoke". **Lo is now an approved OAuth app on `rozed-dev`.**
   - For Lo's own UX: in an org with this restriction, a member who isn't an owner sees "Request"
     instead, and the token can't reach the org's private repos until an owner approves.
4. I clicked Authorize, and the page said "Congratulations, you're all set!"
5. I closed the tab and left the profile window as I found it.

**Poll for the token:**

```sh
curl -X POST -H "Accept: application/json" https://github.com/login/oauth/access_token \
  --data-urlencode client_id=Ov23liBORqwRdiPzqsPP --data-urlencode device_code=<elided> \
  --data-urlencode grant_type=urn:ietf:params:oauth:grant-type:device_code
```

- Polled every 5 s. The responses were `authorization_pending` until approval, then the token,
  which went straight to the file under `umask 077`.
- Fields: `{"token_type":"bearer","scope":"read:org,repo,workflow"}`. Prefix `gho_`. The file is
  `-rw-------`, 41 bytes.

**Verify the token:**

```sh
curl -sI -H "Authorization: token $OAUTH" https://api.github.com/user | grep -i x-oauth-scopes
```

- `HTTP/2 200`
- `x-oauth-scopes: read:org, repo, workflow`
- `x-oauth-client-id: Ov23liBORqwRdiPzqsPP`
- `/user` returns `{"login":"rozed-bot","id":353226052}`

**Conclusion:** device flow works with no client secret. The token is a `gho_` token with exactly
`repo read:org workflow`. In an org that restricts OAuth apps, an owner has to grant Lo access.

## 3. Attachment upload with Lo's OAuth token

**Setup.**

- **Image.** An 8x8 RGB PNG, 165 bytes, made with Python's `zlib` and `struct` (`lo-test.png` in
  the scratchpad).
- **Repo ID.** GraphQL with `$OAUTH`:
  `{"repository":{"databaseId":1447997428,"viewerPermission":"ADMIN","isPrivate":true}}`

**Upload.** The same request `gh --attach` sends:

```sh
curl -X POST "https://uploads.github.com/user-attachments/assets?name=lo-test.png&content_type=image/png&repository_id=1447997428" \
  -H "Authorization: token $OAUTH" -H "Content-Type: application/octet-stream" \
  -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28" \
  -H "User-Agent: lo-spike" --data-binary @lo-test.png
```

- **`HTTP/2 201`**, `content-type: application/json`:
  `{"url":"https://github.com/user-attachments/assets/8f8214ac-43e5-414e-8f71-44210f90a2ee"}`
- There was no redirect. curl computed `Content-Length`.
- I didn't need the fallback test with `$BOT`, since the OAuth upload succeeded.

**Is the returned URL reachable?** (The repo is private.)

| Request | Status |
| --- | --- |
| `HEAD`/`GET`, no auth | **`404`**, `text/plain`, 9 bytes ("Not Found") |
| `HEAD`/`GET`, `Authorization: token $OAUTH` | **`302`** to `https://github-production-user-asset-6210df.s3.amazonaws.com/353226052/697227962-8f8214ac-….png?<signed query>` |
| `HEAD`, `Authorization: token $BOT` | `302` to the same S3 object |
| `HEAD`, `Authorization: Bearer $OAUTH` | `302`. Bearer works for reading. Only `token` was tried for the upload |
| `GET` on the signed S3 URL, without GitHub auth | `200 image/png`, 165 bytes, identical to the uploaded file |

The signed URL has `X-Amz-Expires=300`, so it's valid for 5 minutes. The S3 path is
`<uploader user id>/<asset id>-<uuid>.png`.

**Conclusion:**

- GitHub accepts Lo's OAuth app token (`gho_`, `repo`) for attachment uploads, so the fallback
  to an imported `gh` token isn't needed for this.
- On a private repo the asset URL returns `404` without auth. With the user's token it `302`s to a
  short-lived S3 URL.
- So Lo must fetch attachment images with its token and follow the redirect, and must not send
  the token to S3. A plain `<img src>` won't load in a private repo.

## 4. Stacks on an org repo

**Branches.** I made them with `$BOT` through the REST API instead of `git push`, so the token
never went into a git remote:

1. `POST /git/refs` created `stack-a` at `main`: `201`.
2. `PUT /contents/stack-a.md` on `stack-a` committed `b046244d…`: `201`.
3. `POST /git/refs` created `stack-b` at `b046244d`: `201`.
4. `PUT /contents/stack-b.md` on `stack-b` committed `d712aa76…`: `201`.

`compare main...stack-a` and `compare stack-a...stack-b` both return `ahead_by: 1, behind_by: 0`.

**PRs** (`POST /pulls`, both `201`):

- **#1** "feat: stack layer A": `stack-a` → `main`.
- **#2** "feat: stack layer B": `stack-b` → `stack-a`.
- Both had `"stack": null` on creation.

**Create the stack:**

```sh
curl -X POST -H "Authorization: token $BOT" -H "Accept: application/vnd.github+json" \
  -H "X-GitHub-Api-Version: 2022-11-28" https://api.github.com/repos/rozed-dev/playground/stacks \
  -d '{"pull_requests":[1,2]}'
```

- **`HTTP 201`.** `x-accepted-oauth-scopes` was empty. No opt-in, setting or preview header was
  needed.
- Body (trimmed). The top-level keys are `base, created_at, id, node_id, number, open,
  pull_requests, url`:

```json
{"id":3145564,"number":3,"node_id":"PRS_kwDOVk6v9M4AL_9c",
 "url":"https://api.github.com/repos/rozed-dev/playground/stacks/3","base":{"ref":"main"},"open":true,
 "created_at":"2026-11-06T08:15:21Z",
 "pull_requests":[{"number":1,"state":"open","draft":false,"title":"feat: stack layer A","head":"stack-a","base":"main", …},
                  {"number":2,"state":"open","draft":false,"title":"feat: stack layer B","head":"stack-b","base":"stack-a", …}]}
```

- Each `pull_requests[]` entry has `base, draft, head, html_url, id, merged_at, node_id, number,
  state, title, url, user`.
- **The stack's number is 3,** right after PRs 1 and 2. This confirms stacks share the issue/PR
  number sequence.

**List the stack containing PR #2.** `GET /repos/rozed-dev/playground/stacks?pull_request=2`
returned `200` with both `$BOT` and `$OAUTH`, and the same body:

```json
[{"id":3145564,"number":3,"node_id":"PRS_kwDOVk6v9M4AL_9c","url":".../stacks/3","base":{"ref":"main"},"open":true,
  "created_at":"2026-11-06T08:15:21Z",
  "pull_requests":[{"number":1,"state":"open","draft":false,"merged_at":null,"head":{"ref":"stack-a","sha":"b046244d…"}},
                   {"number":2,"state":"open","draft":false,"merged_at":null,"head":{"ref":"stack-b","sha":"d712aa76…"}}]}]
```

**Other reads:**

- `GET /stacks/3` returned `200` with the full form.
- `GET /pulls/1` has `.stack` = `{"base":{"ref":"main","sha":"6ce218eb…"},"id":3145564,"number":3,"position":1,"size":2}`.
- `GET /pulls/2` has the same with `"position":2`.

**GraphQL.** Both tokens give identical output:

```graphql
{ repository(owner:"rozed-dev", name:"playground") { a: pullRequest(number:1) { ...F } b: pullRequest(number:2) { ...F } } }
fragment F on PullRequest { number stack { id number size baseRefName entries(first:100) { totalCount
  nodes { id position pullRequest { number headRefName baseRefName state isDraft } } } } stackEntry { id position } }
```

- `200`.
- **Both PRs:** `stack` = `{"id":"PRS_kwDOVk6v9M4AL_9c","number":3,"size":2,"baseRefName":"main","entries":{"totalCount":2,"nodes":[{"id":"PRSE_kwDOVk6v9M4AubiH","position":1,"pullRequest":{"number":1,"headRefName":"stack-a","baseRefName":"main","state":"OPEN","isDraft":false}},{"id":"PRSE_kwDOVk6v9M4AubiJ","position":2,"pullRequest":{"number":2,"headRefName":"stack-b","baseRefName":"stack-a","state":"OPEN","isDraft":false}}]}}`
- **PR #1:** `stackEntry` = `{"id":"PRSE_kwDOVk6v9M4AubiH","position":1}`.
- **PR #2:** `stackEntry` = `{"id":"PRSE_kwDOVk6v9M4AubiJ","position":2}`.

**Conclusion:**

- Stacks work on a new private org repo with no opt-in or setting.
- Creating one works with a classic `repo` token.
- Reading works with both the classic token and Lo's OAuth token, over REST (`/stacks`, PR
  `.stack`) and GraphQL (`stack`, `stackEntry`).
- **I didn't try creating a stack with `$OAUTH`.** It has the same `repo` scope, and the endpoint
  lists no required OAuth scope.

The fixtures are left open: PRs #1 and #2, and stack #3.

## 5. Async merge, read-only

Nothing was merged. Afterwards `GET /pulls/1` shows `{"state":"open","merged":false}`.

```sh
curl -H "Authorization: token $BOT" -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28" \
  https://api.github.com/repos/rozed-dev/playground/pulls/1/merge-async/0a95b256-76b8-4eb8-aed6-8ede2a9b9d7f
```

| Request | Status | Body |
| --- | --- | --- |
| Made-up UUID, `$BOT` | `404`, `application/json` | `{"message":"Not Found","documentation_url":"https://docs.github.com/rest/pulls/pulls#get-the-result-of-an-asynchronous-merge","status":"404"}` |
| Same, `$OAUTH` | `404` | identical |
| `merge-async/not-a-uuid`, `$BOT` | `404` | identical, so a malformed UUID isn't a `422` |
| `GET /stacks/9999`, a missing stack, for comparison | `404` | `{"message":"Not Found","documentation_url":"https://docs.github.com/rest/pulls/stacks#get-a-pull-request-stack","status":"404"}` |

- `x-accepted-oauth-scopes` was empty, and `x-github-api-version-selected: 2022-11-28`.
- **`status` is a string (`"404"`), not a number.** Decode it as a string, or ignore it.

**Conclusion:**

- An unknown or expired merge UUID gives GitHub's standard error body
  (`message`, `documentation_url`, `status` as a string) with `404`.
- The same shape comes back for a malformed UUID and for a missing stack, so the client can't tell
  "expired after 24 h" apart from "never existed".
