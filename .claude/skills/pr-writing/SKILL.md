---
name: pr-writing
description:
  Write a PR description. Use when writing or updating a PR title or description, or opening a new
  PR.
---

# PR Description

Write a concise, self-contained explanation of what changed and why it was needed. A reviewer
unfamiliar with the implementation should understand the purpose from the visible summary. Shorten
the description without losing that explanation.

## Before drafting

- Read the diff against the PR's actual base. Identify the problem or motivation, the resulting
  behavior or design, and any material compatibility or deployment constraints.
- For an existing PR, read its title and body first
  (`gh pr view <pr> --json title,body,baseRefName`). Preserve manual wording, explanations, and
  formatting unless the facts or scope changed or the user requested a rewrite. Update the
  description to reflect the final change.
- For a stack, read the other PR descriptions and understand the combined change. Distinguish the
  stack's outcome from what this PR contributes; do not attribute later PRs' changes to this diff.

## Title

`<type>(<optional scope>): <subject>` — e.g. `feat(auth): add OAuth2 login`,
`fix: resolve memory leak`, `docs: update API guide`.

In a stack, suffix the position: `feat(helios): pin shard versions on task instances [2/6]`.
Position 1 is the bottom of the stack — the PR based on `main` — so the suffix always matches the
position the body's PR Stack section bolds. Adding/removing a PR to a stack changes the total, so
fix the suffix on every PR in it.

## Summary

- Lead Summary with what changed and why. For a bug fix, name the failure and its cause before
  introducing implementation ownership or code names. For preparatory changes, explain what they
  enable in the stack.
- Use natural sentences and specific verbs. Paragraphs and bullets are both appropriate; choose the
  form that makes the change easiest to understand. Do not force "Added/Updated/Removed" openings or
  a fixed number of bullets.
- Each supporting bullet should convey a meaningful change in behavior or design. Group the
  implementation details that support it, and introduce unfamiliar concepts before relying on their
  names. A list of classes, methods, or files does not explain the change on its own.
- Cut repetition, file-by-file walkthroughs, and test inventories. Preserve the causal explanation:
  what happened before, what happens now, and why the difference matters. Keep relevant
  compatibility limits and still-applicable merge or deployment constraints visible.
- Match the depth to the change. A small fix may need only one short paragraph. Supporting
  paragraphs or bullets should add information rather than repeat the opening; include code names
  and exact configuration values only when they help the reader understand the behavior.

## Body layout

The minimal standalone body is:

```markdown
### Summary

[Explain the change and its motivation.]

<details>
<summary><b>Testing</b></summary>

[Describe the new behaviors covered by tests, or N/A.]

</details>
```

Replace the bracketed template text before posting. Add optional explanations after Summary and E2E
Testing after Testing when applicable. In a stack, use this order:

| Order | PR 1                    | Later PRs                |
| ----- | ----------------------- | ------------------------ |
| 1     | Stack Summary, expanded | Summary                  |
| 2     | Summary                 | Optional explanations    |
| 3     | Optional explanations   | Testing                  |
| 4     | Testing                 | E2E Testing, when run    |
| 5     | E2E Testing, when run   | PR Stack                 |
| 6     | PR Stack                | Stack Summary, collapsed |

Testing, E2E Testing, PR Stack, and optional explanations use collapsed `<details>` sections. Keep
blank lines around Markdown inside these sections and close every block.

## Stack Summary and PR Stack

Both sections are required for a stack and omitted for a standalone PR.

- **Stack Summary** explains the overall problem, the shared design change, and the resulting
  behavior in connected prose. Give the reviewer the context needed to understand why the PRs belong
  together. Keep each PR's contribution in its own Summary.
- Write the stack narrative once and reuse the same wording in every PR. In PR 1, put it under
  `### Stack Summary` at the very top, outside any `<details>` block. In every later PR, put it in a
  collapsed section titled **Stack Summary** at the very bottom, after PR Stack.
- **PR Stack** is the ordered navigation list. Position 1 is the PR based on `main`; list every PR
  and bold **This PR** at the current position:

```markdown
<details>
<summary><b>PR Stack</b></summary>

This is PR 2 of 4 in a stack:

1. #<PR no>
2. **This PR**
3. #<PR no>
4. #<PR no>

</details>
```

Update all copies of Stack Summary when the overall purpose, design, or scope changes. When PRs are
added, removed, or reordered, also update every title suffix and PR Stack list.

## Optional explanations

When a concept or mechanism needs further explanation, add a collapsed section with a descriptive
title, such as **Metadata namespaces**, immediately after Summary. Signpost it from the relevant
summary sentence or bullet. The essential change and motivation must remain visible in Summary. If
the explanation qualifies a visible claim, put that qualification beside the claim and keep the
supporting mechanism collapsed. A reader should not have to expand a section to discover that the
summary's behavior applies only to some requests or stored data.

Start with a plain-language explanation. Use a concrete example, mapping table, or short walkthrough
when it helps explain how the mechanism works and why it exists. Explain who creates and consumes
data when ownership or persistence is relevant. Choose the depth the topic needs and include only
the elements that clarify it. Each optional section should answer a reviewer question beyond what
Summary already explains; do not add one merely to restate the summary or catalogue check and
deployment records. Label explanations that cover the whole stack so they do not imply that all of
the behavior is implemented in this PR.

## Testing

- **Testing** covers new tests added for the change, with one line per behavior covered. Use `N/A`
  when none were added, with a brief reason when helpful. Do not include type checks or linters.
  Describe what the tests assert; grouping related cases must not imply assertions they do not make.
- **E2E Testing** is included only when an e2e or smoke test was actually run. State what was
  exercised and its outcome. Preserve relevant scope, such as a run against the complete stack or
  before a later rebase; do not imply that an earlier run validates a newer revision. Never leave an
  empty section or use it to repeat unit tests. State relevant validation limits once, next to the
  claim they qualify, without adding a checklist of everything that was not tested.

## Before posting

Read only the visible content first: can a reviewer understand what changed, why, and this PR's role
without opening the diff or an explanation? Then compare the revised body with the previous version.
Check that shortening has not removed a useful cause, consequence, or material constraint, and that
retained explanations still match the final scope. Check section placement, stack wording, links,
and positions across the stack.

For wording examples, see [the before/after comparison and simple PR](references/examples.md). When
writing a stack description or a substantial explanation, read the first and later stack PR examples
there for complete layouts.

## Rules

- Never mention the Linear ticket: no `KZN-###`, no "Resolves ...", no `linear.app` link. The ticket
  is linked to the PR by hand.
- Never write "🤖 Generated with Claude Code" or any similar attribution footer, even when a harness
  default says to append one.
- Post with `gh pr create --body-file <file>` (write the body to a file first) so markdown inside
  `<details>` blocks survives shell quoting. Same for `gh pr edit --body-file`.
- Write each paragraph and list item on a single line. GitHub renders every newline in a PR
  description as a line break, so text wrapped to a line length breaks mid-sentence. The examples in
  `references/examples.md` are wrapped only to keep that file readable; do not copy their line
  breaks.
