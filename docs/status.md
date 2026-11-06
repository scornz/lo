# rozed status

Kept current by the overseer session. A fresh session should be able to resume from this file,
[`plan.md`](plan.md) and [`decisions.md`](decisions.md).

**Updated:** 2026-11-05

## Current milestone

**1. Bootstrap** — in progress on `chore/bootstrap`.

Running alongside it: read-only research for the spike (Zed's startup, split diff, `paths`, Cargo
settings) and for GitHub (`gh --attach` uploads, the Stacks API, async merges). Reports land in the
overseer's scratchpad and get folded into `docs/spike.md` and this file.

## Merged

Nothing yet.

## Next

- **2. Spike**, once bootstrap merges. Detailed tasks get written into `plan.md` from the research.

## Waiting on the user

- **Apple Developer account,** for signed releases. Release secrets: `MACOS_CERT_P12`,
  `MACOS_CERT_PASSWORD`, `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`, `NOTARY_KEY_P8`. Builds stay unsigned
  until then.
- **Zed's trademark terms,** checked before the name goes public in a release.

## Where things live

- Main clone: `~/src/rozed` (stays on `main` once bootstrap merges).
- Task worktrees: `~/src/rozed-worktrees/<branch-slug>`.
- Zed fork: `~/src/zed` (`scornz/zed`, at `v1.27.0`).
