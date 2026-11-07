# GitHub research for Lo: attachments, Stacks API, async merge

Researched 2026-11-05/06. Sources: `cli/cli` at tag `v2.103.3` (commit `63837fb`, the latest release),
`github/gh-stack` at `v0.3.0` (`2a49e67`), GitHub's REST OpenAPI description
(`github/rest-api-description` `main`, `descriptions/api.github.com/api.github.com.json`, fetched
2026-11-05), `github/docs` `main` (`17d6ee5`), live GraphQL introspection, and read-only REST GETs.
Nothing was written to GitHub. **[inferred]** marks a conclusion I drew rather than read.

---

## 1. Attachments (`gh --attach`)

### Summary

`gh` does **one request per file**. There's no policy step, no S3 upload and no confirmation:

```
POST https://uploads.github.com/user-attachments/assets?name=<basename>&content_type=<mime>&repository_id=<numeric repo id>
Authorization: token <the user's github.com token>
Content-Type: application/octet-stream
Content-Length: <file size>
Accept: application/vnd.github+json
X-GitHub-Api-Version: 2022-11-28
User-Agent: GitHub CLI <version>

<raw file bytes>

→ 201 {"url": "https://github.com/user-attachments/assets/<uuid>"}
```

Then `gh` puts `![alt](url)` in the body for an image, or the bare URL as its own paragraph for a
video.

### Where it is in the code

| What | Source (`cli/cli@v2.103.3`) |
| --- | --- |
| Package overview and the "upload cannot be undone" contract | `internal/attachments/doc.go:1-50` |
| `NewUploader`: host check, then repo ID, then permission | `internal/attachments/client.go:29-55` |
| GHES refused (`auth.IsEnterprise`); ghe.com tenants allowed | `internal/attachments/client.go:57-64` |
| Permission allowlist `ADMIN`, `MAINTAIN`, `WRITE` ("READ and TRIAGE get a 404") | `internal/attachments/client.go:66-84` |
| Request: URL, query params, headers, body, `ContentLength`, `GetBody` for redirects | `internal/attachments/client.go:101-151` |
| Error mapping: 404, 422, 429 with `Retry-After` | `internal/attachments/client.go:165-199` |
| Upload host `https://uploads.<host>/` | `internal/ghinstance/host.go:93-100` |
| Auth header `Authorization: token %s`, not sent on a redirect to another host | `api/http_client.go:230-256` (header at line 251) |
| Default headers `User-Agent`, `X-GitHub-Api-Version: 2022-11-28` | `api/http_client.go:84-93`, `api/client.go:19-25` |
| `uploads.github.com` normalizes to `github.com`, so the github.com token is sent | `cli/go-gh@v2.17.0` `pkg/auth/auth.go:178-195`; test `internal/attachments/client_test.go` `TestUploadAuthentication` |
| Exact request asserted (URL, headers, body, length) | `internal/attachments/client_test.go` `TestUpload` (lines ~63-91) |
| File types, size limits, alt text, Markdown | `internal/attachments/userasset.go:14-215` |
| Flag, `<file>#<alt>` parsing, 50-file cap | `internal/attachments/flags.go:13-123` |
| Upload loop (stop at first failure) and appending | `internal/attachments/attach.go:117-144`, `239-270` |
| Rewriting existing references, video player vs link | `internal/attachments/references.go:97-172` |
| Repo ID and permission come from the PR's `repository{databaseId,viewerPermission}` | `api/query_builder.go:411`, `api/queries_pr.go:328-345`, `pkg/cmd/pr/shared/commentable.go:173` |
| `pr create` uses the **base** repo | `pkg/cmd/pr/create/create.go:536` |

### Request details

- **Method and URL.** `POST https://uploads.github.com/user-attachments/assets`. For a ghe.com
  tenant it's `https://uploads.<tenant>.ghe.com/...`. GHES has no such endpoint.
- **Query parameters.**
  - `name`: the file's basename.
  - `content_type`: picked from the file extension.
  - `repository_id`: the repo's numeric REST ID, which is the GraphQL `Repository.databaseId`.
- **Body.** The raw bytes. There's no multipart form or JSON.
- **Headers.** `Content-Type: application/octet-stream` and `Accept: application/vnd.github+json`.
  The request is built by hand so it can set `Content-Length` and `GetBody`. PR #14180 says "The
  endpoint requires both a content type and a content length on every request."
- **Response.** `201` with `{"url":"https://github.com/user-attachments/assets/<id>"}`. `gh` reads
  only `url` and errors if it's empty.
- **Redirects.** `gh` sets `GetBody` because "Without GetBody a redirect re-reads an exhausted
  reader" (`client.go:129-130`). That suggests the endpoint may redirect, though I didn't observe
  one. The auth header isn't forwarded to a different host.
- **Errors.**
  - `404` means no write access **or** a token the endpoint won't take. The two can't be told
    apart. `gh` reports both as "attaching files requires write access to the repository"
    (`client.go:179-182`).
  - `422` carries a message.
  - `429` may carry `Retry-After`.

### Which token, and how it's sent

- **Which token.** The token of the active `gh` account for `github.com`. The upload host
  normalizes to `github.com`.
- **Header scheme.** `Authorization: token <token>`. Issue cli/cli#14495 (whose reporter used
  curl) says the endpoint "wants `Authorization: token`, not `Authorization: Bearer`. `Bearer`
  answers `404 Not Found`." Some third-party scripts send `Bearer` and claim it works, so the
  reports conflict. **Use `token`, as `gh` does.**
- **No cookies.** The browser's drag-and-drop flow is a different, multi-step flow:
  1. `POST github.com/upload/policies/assets`
  2. an S3 form upload
  3. `PUT github.com/upload/assets/:id`

  That flow needs the `user_session` cookie and a CSRF token. With a PAT it answers 422 (cli/cli
  #13256). `gh` doesn't use it.

### Which tokens the endpoint accepts

The endpoint, not `gh`, decides.

1. **History.**
   - The original stack, PR #14180, had a client-side allowlist: OAuth (`gho_`), classic PAT
     (`ghp_`) and fine-grained PAT (`github_pat_`).
   - v2.102.0 (PR #14516) added `ghu_`, GitHub App user-to-server tokens.
   - v2.103.2 (PR #14804) added `ghs_`, installation tokens.
   - v2.103.2 (PR #14805) then removed the token check altogether: "The credential is left to the
     endpoint, which accepts tokens from only some GitHub Apps" (`client.go:31-33`).
2. **What maintainers said.**
   - williammartin, on #14495: "generally, `ghu_` tokens are unsupported by the API. However,
     there are a number of privileged apps that mint tokens that the API accepts."
   - BagToad, on #14495 (2026-10-14): "currently only first party apps like Copilot Cloud Agent can
     mint tokens that satisfy this API endpoint. Unfortunately at this time there's no
     configuration that can be turned on to permit a user's GitHub App to upload attachments."
     The tracking issue for third-party GitHub Apps is #14309 (open). There, `ghs_` and Actions
     `GITHUB_TOKEN` get `404 {"message":"Not Found"}` even with write access, and BagToad called it
     "intended behavior for now."
   - Amp's `ghu_` token, which had MAINTAIN access, got the same 404 (comment on #14495).
3. **Third-party OAuth apps.** I found nothing that says whether a `gho_` token from **an OAuth
   app other than gh's** is accepted.
   - All the evidence and restrictions I found are about GitHub Apps (`ghu_`/`ghs_`).
   - `gh` treated every `gho_` token as allowed. It can't tell which OAuth app minted one.
   - PATs work, so the endpoint isn't limited to gh's own app.

   **[inferred]** An OAuth app's user token with `repo` scope probably works, since it's a plain
   user credential like a PAT. This needs a real test: one upload with Lo's own OAuth token to
   a `rozed-dev` repo where the account has WRITE. A refusal would show up as a 404.

   Lo's plan to fall back to an imported `gh` token is sound, since that token is gh's own
   `gho_`. **If Lo were ever built as a GitHub App rather than an OAuth app, uploads would be
   refused.**
4. **Scopes.** Nothing says which OAuth scope is needed. **[inferred]** `repo` for private repos;
   Lo already asks for it.

### What an upload needs

- **Repo ID.** The numeric `repository_id`, which is GraphQL `databaseId` or REST `id`. `gh` uses
  the PR's own repository, the base repo, never the head fork.
- **Write access.** `viewerPermission` must be `ADMIN`, `MAINTAIN` or `WRITE`. `READ` and `TRIAGE`
  get 404s. That's narrower than the web UI, which lets commenters upload (open issues #14302 and
  #14939, the latter about fork contributors).
- **Empty `viewerPermission`.** GraphQL returns none for installation tokens, so `gh` lets an empty
  value through and leaves it to the endpoint (`client.go:74-78`).
- **Host.** github.com or ghe.com, not GHES.

### Size and type limits

| | gh client check | GitHub docs ("Attaching files") |
| --- | --- | --- |
| Images, GIFs | ≤ 10 MiB (`10*1024*1024`) | 10 MB |
| Videos | ≤ 100 MiB ("the server refuses the rest") | 10 MB on free plans, 100 MB on paid (for videos over 10 MB on a paid repo, you must be an org member, outside collaborator, or on a paid plan) |
| Types | `.png .jpg .jpeg .gif .webp .svg .mp4 .mov .webm`, by extension only. "the endpoint accepts mislabeled bytes anyway" | Same image and media set. The web UI also takes other files up to 25 MB, but gh only does images and video |
| Count | At most 50 `--attach` values per command | — |
| Empty file | Refused before upload | — |

Sources: `userasset.go:14-37`, `userasset.go:141-145`, `userasset.go:169-185`, `flags.go:15`, and
`content/get-started/writing-on-github/working-with-advanced-formatting/attaching-files.md`
lines 38-47 in `github/docs`.

**Uploads can't be undone.** "there is no endpoint to delete one" (`attach.go:79-81`).

### The Markdown it inserts

**Images.**

- **Format.** `![<alt>](<url>)` (`userasset.go:88-90`).
- **Alt text.** Comes from `--attach 'path#alt'`. Otherwise it's the filename with its extension
  removed and remaining dots replaced by spaces, which "mirrors the web uploader": `login.screen.png`
  becomes `login screen` (`userasset.go:66-79`).
- **Escaping.** `\`, `[` and `]` are escaped, and newlines become spaces (`userasset.go:190-200`).
- **Check.** The image is re-parsed to prove the alt text can't break out (`userasset.go:202-215`).

**Videos.**

- **Format.** The bare URL, which GitHub turns into a player when the URL is a paragraph on its
  own (`userasset.go:92-118`).
- **No alt text.** Videos can't take any.
- **References.** An existing `![](./v.mp4)` that stands alone in its paragraph is replaced
  entirely by the URL. Inside a sentence it degrades to a link, `[name](url)`
  (`references.go:135-171`).

**Placement.**

- **Existing references.** A reference to the local path is rewritten in place, and only the
  destination changes (`references.go:139-142`).
- **Unreferenced files.** Appended in order, each as its own paragraph joined with `"\n\n"`
  (`attach.go:243-270`).

**Commands.** `--attach` exists on these commands:

- `gh issue create`, `issue edit` and `issue comment`
- `gh pr create`, `pr edit` and `pr comment`
- `gh issue artifact create` and `issue artifact edit`

It's **not on `gh pr review`**: `pkg/cmd/pr/review/review.go` has no attach flag.

**[inferred]** The asset is tied to a repository, not a comment, so the same URL works in review
comments and review summaries. That matches the web UI.

---

## 2. Stacks API

### Status and opt-in

- **Status.** Stacked PRs are now **generally available**, not in preview:
  - Public preview began 2026-07-30
    (<https://github.blog/changelog/2026-07-30-stacked-pull-requests-are-now-in-public-preview/>).
  - GA came 2026-10-06
    (<https://github.blog/changelog/2026-10-06-stacked-pull-requests-generally-available>):
    "available on all github.com plans and will be included in an upcoming GitHub Enterprise
    Server release."
  - The docs commit is `77764b423` "Stacked PRs GA (#63609)".
- **No opt-in.** The docs say: "Stacked pull requests require **no setup or enablement**. If your
  team already uses pull requests, they can create a stack today."
  (`content/pull-requests/tutorials/roll-out-stacked-prs.md:15`)
- **No repo setting.** Neither REST nor GraphQL exposes a repository or org setting for stacks. I
  searched every `stack` key in the OpenAPI description and the `Repository` GraphQL fields.
- **404 handling.** `gh-stack` still treats a 404 from `/stacks` as "Stacked PRs are not enabled
  for this repository" (`cmd/utils.go:78-82`, `internal/github/github.go:510-512`); that comes from
  the gradual rollout. A third-party client reported the same 404 for repos that weren't enabled
  during the preview.

  **[inferred]** For Lo, read a 404 on `GET /repos/{o}/{r}/stacks` as "stacks unavailable here"
  (GHES, or a token without access), not as a repo setting. Design §4's "when the repo doesn't have
  stacks turned on" should become "when the Stacks API isn't available."
- **Constraints.**
  - Stacks are same-repo only; cross-fork stacks aren't supported.
  - A stack is a linear chain.
  - GitHub Desktop doesn't support stacks.
  - At GA, a stack whose trunk branch is deleted is retargeted automatically, which "supports
    workflows where one stack branches off another" (GA changelog). **[inferred]** A tree can be
    modeled as several linear stacks, where a child stack's trunk is a layer's head branch. A PR
    can be in only one stack.

### REST endpoints

From OpenAPI `api.github.com.json`. All are tagged `x-github.subcategory: "stacks"`,
`enabledForGitHubApps: true` and `githubCloudOnly: false`. They're present in both the
`2022-11-28` and `2026-03-10` API versions. Docs:
<https://docs.github.com/rest/pulls/stacks>.

| Method and path | operationId | Fine-grained / App permission |
| --- | --- | --- |
| `GET /repos/{owner}/{repo}/stacks` | `pull-request-stacks/list` | `pull_requests: read` |
| `POST /repos/{owner}/{repo}/stacks` | `pull-request-stacks/create` | `pull_requests: write` |
| `GET /repos/{owner}/{repo}/stacks/{stack_number}` | `pull-request-stacks/get` | `pull_requests: read` |
| `POST /repos/{owner}/{repo}/stacks/{stack_number}/add` | `pull-request-stacks/add` | `pull_requests: write` |
| `POST /repos/{owner}/{repo}/stacks/{stack_number}/unstack` | `pull-request-stacks/unstack` | `pull_requests: write` |

The permission column comes from `github/docs`
`src/github-apps/data/fpt-2022-11-28/fine-grained-pat-permissions.json`. A live GET returned
`X-Accepted-GitHub-Permissions: pull_requests=read` and an empty `X-Accepted-OAuth-Scopes`.

**List.**

- **Query parameters.** `pull_request` (integer) filters to the stack containing that PR. Also
  `per_page` (max 100, default 30) and `page`.
- **`200` body.** An array of `pull-request-stack-minimal`:
  - `id`: int
  - `number`: int
  - `node_id`: string, prefix `PRS_`
  - `url`
  - `base`: `{ref}`
  - `open`: bool. "False when all pull requests are merged or closed"
  - `created_at`
  - `pull_requests`: `[{number, state: open|closed, draft, merged_at|null, head: {ref, sha}}]`,
    ordered bottom to top
- **Other responses.** `404` and `422`.

**Create.**

- **Body.** `{"pull_requests": [101, 102, 103]}`, required, from bottom to top, with
  `minItems: 2` and `maxItems: 100`. "Each pull request's base ref must match the previous pull
  request's head ref."
- **`201` body.** `pull-request-stack`. It has the same top-level fields as the minimal version,
  but each `pull_requests[]` entry is `pull-request-minimal` (`id`, `number`, `url`,
  `head{ref,sha,repo{id,url,name}}`, `base{ref,sha,repo{…}}`) plus `node_id`, `title`, `state`,
  `merged_at`, `draft`, `html_url` and `user` (a simple user).
- **`422` examples.**
  - Missing PRs: `errors[{resource:"Stack", field:"pull_requests", code:"missing", value:[123]}]`.
  - Not linear: message "Pull requests must form a stack, where each PR's base ref is the previous
    PR's head ref".
  - Already stacked: message "Pull request #123 is already part of a stack".
- **Other responses.** `404`.

**Get.** `200` returns `pull-request-stack`, the full form as for create. `404` otherwise.

**Add.**

- **Body.** `{"pull_requests":[…]}` with `minItems: 1` and `maxItems: 100`. Send only the new PRs,
  from the current top upward. The first one's base must equal the current top's head.
- **Responses.** `200` with the full stack. `404`. `409` "Stack is currently being modified".
  `422` for missing PRs or PRs that can't be appended.

**Unstack.**

- **Body.** None. "Removes the unmerged pull requests from a stack. Pull requests that cannot be
  unstacked (for example, those that are queued for merge) are left in place."
- **Responses.**
  - `200` with the remaining stack when some PRs remain.
  - `204` when the stack is dissolved.
  - `409` when another request is modifying it.
  - `422` when every PR is locked.
- The docs add that unstacking removes open, draft and closed PRs; merged and queued PRs stay.

There's no endpoint to reorder or remove a single layer. To restructure, unstack and recreate.

**Stack on PR resources.**

- **Field.** `pull-request` and `pull-request-simple` both have `stack` (`pull-request-stack`,
  nullable). So `GET /pulls` and `GET /pulls/{n}` include it.
- **Shape.** `{base:{ref,sha}, size, position, id, number}`. `position` is 1-based, with 1 at the
  bottom.
- **Example.** Observed on cli/cli#14952:
  `{"base":{"ref":"trunk","sha":"eeb44bc…"},"id":3129431,"number":14958,"position":2,"size":8}`.

**Webhooks.**

- **Stack field.** Every `pull_request` action payload carries `pull_request.stack`.
- **New action.** `stacked` (`webhook-pull-request-stacked`), "A pull request was added to a
  stack", with a top-level `stack`.
- **Actions.** Workflows can read `github.event.pull_request.stack`.

**[inferred]** Stack numbers share the repo's issue/PR number sequence:

- cli/cli stack `14958` sits between PRs 14957 and issue 14959, and `GET /issues/14958` is 404.
- github/gh-stack stack `580` sits between PR 579 and issue 581, and `GET /issues/580` is 404.

Don't assume a stack number is a PR number.

### GraphQL (live introspection, read-only)

- `PullRequest.stack: PullRequestStack`: "The stack this Pull Request belongs to, or null if it is
  not part of a stack".
- `PullRequest.stackEntry: PullRequestStackEntry`: "The stack entry for this Pull Request, or null
  if it is not part of a stack".
- `PullRequestStack` fields:
  - `id: ID!` (`PRS_…`)
  - `number: Int!`
  - `size: Int!`
  - `baseRefName: String!`
  - `entries(first/last/after/before): PullRequestStackEntryConnection!` (`nodes`, `edges`,
    `pageInfo` and `totalCount`)
- `PullRequestStackEntry` fields:
  - `id: ID!` (`PRSE_…`)
  - `position: Int!`. "1 is the closest to the base branch".
  - `pullRequest: PullRequest`
  - `stack: PullRequestStack`
- Timeline: `AddedToStackEvent` and `RemovedFromStackEvent`, each with `actor`, `createdAt`,
  `pullRequest`, `stackId: BigInt` and `stackNumber: Int`. The `PullRequestTimelineItemsItemType`
  values are `ADDED_TO_STACK_EVENT` and `REMOVED_FROM_STACK_EVENT`.
- **No stack mutations.** The docs say: "The GraphQL API is read-only for stacks; there are no
  stack mutations."
- `mergePullRequest` says "This mutation does not support stacked pull requests" and points to the
  async REST API.
- `enablePullRequestAutoMerge` and `enqueuePullRequest` say nothing about stacks. Auto-merge for
  stacks was "rolling out over the next few weeks" at GA (2026-10-06), and I found no documented
  API for it. **[unknown]**
- No `Repository` field mentions stacks.

This query works today, and returned 8 entries on cli/cli#14952:

```graphql
pullRequest(number: N) {
  stack { id number size baseRefName
          entries(first: 100) { totalCount nodes { id position pullRequest { number headRefName baseRefName state isDraft } } } }
  stackEntry { id position }
}
```

### Rules for stacks

Sources: `content/pull-requests/reference/stacked-pull-requests.md` and
`…/merging-stacked-pull-requests.md` in `github/docs`.

- Each PR is evaluated against the **stack base**: reviews, required checks, CODEOWNERS, code
  scanning and deployments.
- To merge PR N, every PR below it must also meet the requirements, and the stack must have
  **fully linear history**. Otherwise the web UI shows "Rebase stack", a server-side cascading
  rebase that produces signed commits and keeps approvals when the diff is unchanged.
- Merges go bottom-up and contiguously. Merging N merges every unmerged PR below it. The PRs above
  are retargeted and rebased automatically.
- Merge, squash and rebase are all supported. Squash gives "n squashed commits".
- Merge queue: the stack is added in order. If one PR is ejected, every PR above it goes too. A
  merge group may exceed its maximum size by up to 50% to keep a stack together.
- **Bypass rules: "only the bottom pull request can be merged this way. You cannot merge the whole
  stack with bypass rules."**
- **Atomicity: the docs disagree.**
  - The API reference says "A stack merge request is atomic, meaning either the whole group of pull
    requests merges, or is added to the merge queue, or none of it is."
  - "Troubleshooting stacked pull requests" says "If a failure occurs part way through, merging
    stops at that pull request. Pull requests below it that merged successfully remain landed."

  Lo's design (§9, "they merge, join the queue or don't merge at all") should allow for a
  partial result. Re-read the stack after a `failed` result.

### Live check

- **rozed-dev.** `gh` in this devbox runs as a GitHub App user-to-server token (`ghu_…`, OAuth
  client `Iv23li4a0IB15wgtu8tY`) minted by Coder external auth (`~/.local/bin/gh` wrapper), as user
  `scornz`.
  - `gh api orgs/rozed-dev` works: the org was created 2026-11-06T07:17:47Z and has 0 public repos.
  - `gh api orgs/rozed-dev/repos` (also with `type=all`) returns `[]`.
  - `user/memberships/orgs/rozed-dev` returns **403** "You do not have access to this organization
    membership."
  - `user/orgs` is empty.

  So **this account's token can't see any rozed-dev repo, and I couldn't test the endpoint
  there.** **[inferred]** Most likely the Coder GitHub App isn't installed on the org, or the repos
  are private.
- **Elsewhere,** every probe returned `200` with `X-Accepted-GitHub-Permissions: pull_requests=read`:
  - `GET repos/scornz/lo/stacks` (public, user-owned) returned `[]`.
  - `GET repos/cli/cli/stacks` returned real stacks, including #14958 with 8 draft PRs.
  - `GET repos/github/gh-stack/stacks` returned stacks too.

  The endpoint is live without any opt-in, and draft PRs can be stacked.
- **Async merge.** `GET repos/scornz/lo/pulls/1/merge-async/<zero-uuid>` returned `404` with
  `X-Accepted-GitHub-Permissions: contents=write`.

---

## 3. Async merge

Both endpoints are in OpenAPI tag `pulls`, subcategory `pulls`, and in both API versions. Docs:
<https://docs.github.com/rest/pulls/pulls#merge-a-pull-request-asynchronously> and
`#get-the-result-of-an-asynchronous-merge`. The docs link `?apiVersion=2026-03-10`, but the
`2022-11-28` description has the endpoints too.

### Submit: `PUT /repos/{owner}/{repo}/pulls/{pull_number}/merge-async`

operationId `pulls/merge-async`. It needs `contents: write`; **[inferred]** for an OAuth app,
`repo` scope. `triggersNotification: true`.

- **What it does.** "Merges a pull request into the base branch in the background or adds it to a
  merge queue." It's "the required API for merging stacked pull requests. For a stacked pull
  request, the operation includes all open downstack pull requests." It works for unstacked PRs
  too.
- **Body.** Optional JSON; the whole body is nullable.
  - `commit_title`: string. "Only supported for direct merges".
  - `commit_message`: string. Direct merges only.
  - `sha`: string. The head must match. If omitted, the head at request time is used, and "if the PR
    is pushed in between the merge being requested and being executed, the merge will be
    cancelled".
  - `merge_method`: `merge` | `squash` | `rebase`. Direct merges only.
  - `merge_action`: `default` | `direct_merge` | `merge_queue`. If omitted, it's `default`, which
    uses the queue if the target branch has one and merges directly otherwise. `gh-stack` notes
    that an explicit `merge_queue` on a branch without a queue is rejected rather than silently
    merging directly (`internal/github/merge_async.go:22-27`).
  - `bypass_rules`: boolean, default `false`.
- **Responses.** Each body is `pull-request-merge-async-result` unless noted.
  - **`202`, accepted.**
    `{"status":"pending","details":{"message":"Merge request enqueued.","uuid":"630b9d5e-…","merge_method":"squash","merge_action":"direct_merge","expected_head_sha":"6358cd…"}}`
  - **`200`, already merged or queued.**
    `{"status":"merged","details":{"message":"Pull request is already merged.","sha":"…"}}` or
    `{"status":"enqueued","details":{"message":"Pull request is already in the merge queue."}}`.
  - **`409`, a request is already pending for this PR.** The body returns **that** request's UUID:
    `{"status":"pending","details":{"message":"A merge request already exists for this pull request.","uuid":"…","merge_method":"squash","merge_action":"default","expected_head_sha":"…"}}`.
  - **`400`, basic state check failed** (closed, draft):
    `{"status":"failed","details":{"message":"Pull request is closed."}}`. "Branch protection rules
    and repository rules are not run at this stage."
  - **`403`, `404`, `422`.** These use standard error bodies. `gh-stack` reads a `404` as "async
    stack merge is not available for this repository" (`merge_async.go:34-37`, `299-322`).

### Poll: `GET /repos/{owner}/{repo}/pulls/{pull_number}/merge-async/{uuid}`

operationId `pulls/get-merge-async-result`. It also needs `contents: write`, confirmed by a live
header.

- **Responses.** `200` with `pull-request-merge-async-result`. `403` or `404` otherwise.
- **Retention.** Results are kept for **24 hours** after their last update; after that the UUID
  returns `404`.
- **Examples.**
  - Pending: `{"status":"pending","details":{"message":"Merge request is in progress.","uuid":"…","merge_method":"merge","merge_action":"default","expected_head_sha":"…","bypass_rules":false}}`
  - Enqueued: `{"status":"enqueued","details":{"message":"Pull request is in the merge queue."}}`
  - Merged: `{"status":"merged","details":{"message":"Pull request was merged.","sha":"6dcb09b…"}}`
  - Failed: `{"status":"failed","details":{"message":"Merge conflict: the pull request could not be merged."}}`

### Result schema (`pull-request-merge-async-result`)

- **`status`** (required) is one of:
  - `pending`: running in the background.
  - `merged`: merged into the base. `details.sha` is the merge commit OID.
  - `enqueued`: added to the merge queue. **This is final for queue requests and doesn't mean the
    PR merged.** "This result does not change when the merge queue later merges the pull request."
    Check `GET /pulls/{n}/merge`, or GraphQL `merged` / `mergeQueueEntry`.
  - `failed`: `details.message` says why. Branch-rule failures arrive here, late.
- **`details`** (required) is `anyOf`:
  - Pending: `message`, `uuid`, `merge_method` (`default|merge|squash|rebase`), `merge_action`
    (`default|merge_queue|direct_merge`), `expected_head_sha`, and optionally `bypass_rules`.
  - Queued: `message`.
  - Failed: `message`.
  - Merged: `message` and `sha`.
- **Polling interval.** The API doesn't specify one. `gh-stack` polls every 1 s, up to 600 times
  (`cmd/merge.go:418-440`).

### Related

- `PUT /pulls/{n}/merge` (sync) says: "We recommend using the asynchronous merge API instead. This
  endpoint does not support stacked pull requests or merging with a merge queue." The GraphQL
  `mergePullRequest` mutation says the same.
- Stacks and bypass: only the bottom PR can be merged with bypass rules. Lo's admin override
  (§9) can't apply to a whole stack.

---

## Notes against the design doc

- **§8 Attachments.** "the way `gh --attach` does" is a single token-authenticated POST, not a
  policy, upload and confirm sequence.
  - The 10 MB and 100 MB limits match.
  - So does needing write access: READ and TRIAGE get 404.
  - "If GitHub won't take Lo's sign-in": the documented refusals are GitHub App tokens.
    Lo's OAuth app token is untested. Test it with one upload before relying on the fallback.
  - A refused token and missing write access both return the same 404.
  - Uploads can't be deleted.
- **§4 "repo doesn't have stacks turned on".** There's no such setting on github.com since GA. The
  API is unavailable only on GHES, or where it returns 404.
- **§4 "GitHub stacks can't branch".** Correct: create requires a linear chain, and a PR can be in
  only one stack. Stacks also need at least 2 PRs.
- **§9 stacks atomic.** The API reference says atomic; the troubleshooting doc describes a partial
  merge. Handle both.
- **§3 REST covers the Stacks API and async merges.** Matches. Stack *membership* is also in
  GraphQL (`stack`, `stackEntry`) and on REST PR objects (`stack`), so polling doesn't need the
  Stacks API.

## Sources

- cli/cli v2.103.3: `internal/attachments/*.go`, `internal/ghinstance/host.go`,
  `api/http_client.go`, `api/client.go`, `api/query_builder.go`, `api/queries_pr.go`,
  `pkg/cmd/pr/shared/commentable.go`, `skills/gh/SKILL.md:97-148`
- cli/cli PRs and issues: #14177, #14180, #14186, #14516, #14804, #14805, #14495, #14309, #14302,
  #14939, #13256; release notes v2.99.0, v2.102.0, v2.103.2
- go-gh v2.17.0 `pkg/auth/auth.go:178-195`
- github/gh-stack v0.3.0: `internal/github/merge_async.go`,
  `internal/github/github.go:430-622`, `cmd/merge.go:418-440`, `cmd/utils.go:78-82`
- REST OpenAPI: <https://github.com/github/rest-api-description/blob/main/descriptions/api.github.com/api.github.com.json>.
  Paths `/repos/{owner}/{repo}/stacks*` and `/repos/{owner}/{repo}/pulls/{pull_number}/merge-async*`;
  schemas `pull-request-stack`, `pull-request-stack-minimal`, `pull-request-stack-pull-request`,
  `pull-request-merge-async-result`, `webhook-pull-request-stacked`
- Docs:
  - <https://docs.github.com/rest/pulls/stacks>
  - <https://docs.github.com/en/pull-requests/reference/stacked-pull-requests-apis-and-webhooks>
  - <https://docs.github.com/en/pull-requests/reference/stacked-pull-requests>
  - <https://docs.github.com/en/pull-requests/get-started/about-stacked-prs>
  - <https://docs.github.com/en/pull-requests/tutorials/roll-out-stacked-prs>
  - …/merging-stacked-pull-requests, …/troubleshooting-stacked-pull-requests
  - <https://docs.github.com/en/github-cli/github-cli/attaching-files-with-github-cli>
  - <https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files>
- Changelog:
  - <https://github.blog/changelog/2026-07-30-stacked-pull-requests-are-now-in-public-preview/>
  - <https://github.blog/changelog/2026-10-06-stacked-pull-requests-generally-available>
- Permission data: `github/docs` `src/github-apps/data/fpt-2022-11-28/fine-grained-pat-permissions.json`
- GraphQL introspection of `PullRequest`, `PullRequestStack`, `PullRequestStackEntry`,
  `AddedToStackEvent`, `RemovedFromStackEvent`, `Repository` and `Mutation`
