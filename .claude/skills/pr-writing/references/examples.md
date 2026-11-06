# PR description examples

These examples illustrate wording and layout. The stack examples are shortened adaptations of
[PR 1](https://github.com/kzn-code/atlas/pull/2789) and
[PR 6](https://github.com/kzn-code/atlas/pull/2794) of the protocol stack. Their validation
statements describe those examples; use evidence from the actual change when writing another PR.

## Explain the change before shortening

An implementation inventory:

> Added `ProtocolProxy` and `ModelProxy`, removed the provider HTTP proxies, and updated
> `build_proxy`.

An explanation of the same change:

> Models sharing an inference endpoint could inherit the first model's computer-tool settings. Each
> request now carries the selected model's configuration, so shared HTTP connections use the correct
> settings for every model.

The second version explains the previous failure, the change, and its consequence. The class names
can support that explanation when a reviewer needs them.

Keep a concrete reason when shortening. In the Responses PR, this sentence explains the benefit of
separating the computer-tool formats:

> Grok's second turn failed with a 422 because computer results used OpenAI's native format. xAI now
> receives matching function calls and results.

## Simple PR

Title: `fix(helios): preserve each model's computer-tool settings`

```markdown
### Summary

Models sharing an inference endpoint could inherit the first model's computer-tool settings. Each
request now carries the selected model's configuration, so shared HTTP connections use the correct
settings for every model.

<details>
<summary><b>Testing</b></summary>

- Models sharing an endpoint retain their own model identity and computer-tool settings.

</details>
```

## First PR in a stack

Title: `refactor(helios): introduce abstract protocols and concrete provider implementations [1/7]`

````markdown
### Stack Summary

_Inference providers can share an API format while requiring different authentication, computer
tools, and replay behavior. The old design split this logic between protocols and inference
adapters, allowing providers to inherit assumptions specific to Anthropic or OpenAI._

_This stack removes the inference adapter layer and introduces shared Messages and Responses base
classes with concrete provider implementations. Shared code handles request and response
translation; each provider owns its settings and behavior. Model configuration explicitly selects
the protocol and capabilities._

_Conversation replay also uses the current content blocks, so edits affect the next request.
Additional state needed by a provider is stored on the relevant blocks and interpreted by that
provider's implementation._

### Summary

- Introduces an abstract protocol interface and shared `BaseMessages` and `BaseResponses` classes,
  with concrete implementations for Anthropic, Fireworks, OpenAI, and xAI. These give providers that
  share an API format a place to define their own endpoints, authentication, and behavior.
- Passes the selected model's configuration on every request, preventing models sharing an HTTP
  connection pool from inheriting another model's computer-tool settings.
- Preserves old protocol names as aliases and introduces distinct provider metadata namespaces with
  support for reading older keys. See **Metadata namespaces** below for how the stack uses them.

<details>
<summary><b>Metadata namespaces</b></summary>

A metadata namespace is a provider's name used as a key in an internal JSON object. It identifies
additional information that Helios carries for that provider alongside the common message fields.

For example, this text block contains readable text plus information needed to replay an OpenAI
message:

```json
{
  "type": "text",
  "text": "I'll check the page.",
  "provider_meta": {
    "openai": {
      "message": {
        "id": "msg_123",
        "phase": "commentary"
      }
    }
  }
}
```

Here, `openai` is the namespace. Helios displays the common `text` field; the OpenAI implementation
uses the saved message identity and phase to preserve message boundaries and distinguish progress
updates from final answers when constructing a later request.

Across this stack, namespaces are used in two places. `InferenceRequest.provider_options` carries
settings for the request being sent, such as beta flags. A block's `provider_meta` preserves state
needed to reconstruct it on a later turn. Each concrete provider creates and interprets its own
payloads; the shared block schema supplies the container.

The stack separates provider keys while continuing to read older data:

| Concrete protocol | Namespace for new data | Older namespace also accepted |
| ----------------- | ---------------------- | ----------------------------- |
| `ant_messages`    | `anthropic`            | None                          |
| `fw_messages`     | `fireworks`            | `anthropic`                   |
| `oai_responses`   | `openai`               | None                          |
| `xai_responses`   | `xai`                  | `openai`                      |

</details>

<details>
<summary><b>Testing</b></summary>

- Metadata lookup prefers the current provider key, accepts legacy keys, and respects cleared data.
- Only complete concrete provider implementations are selectable; legacy aliases still resolve.
- Shared connections preserve each model's identity and computer-tool settings.

</details>

<details>
<summary><b>E2E Testing</b></summary>

Recorded before this rebase: live computer-use rollouts with Claude Sonnet 5, Claude Opus 5,
GPT-5.5, and Grok 4.6 passed with this PR as the base of the stack.

</details>

<details>
<summary><b>PR Stack</b></summary>

This is PR 1 of 7 in a stack:

1. **This PR**
2. #2790
3. #2791
4. #2792
5. #2793
6. #2794
7. #2795

</details>
````

## Later PR in a stack

Title: `refactor(helios): replay conversations from current content blocks [6/7]`

```markdown
### Summary

Replay could ignore edits to conversation blocks because it preferred a duplicate raw response. This
PR builds subsequent requests from the current blocks, with the provider's additional replay state
stored on those blocks.

- Reasoning and tool blocks retain their provider metadata; `OpaqueBlock` preserves items such as
  compaction markers that have no ordinary text or tool representation.
- Edited function calls are rebuilt from their current fields. Edited native computer calls fail
  explicitly if their saved provider state is no longer valid.
- Compaction follows only markers still present in the blocks, retaining the latest marked turn and
  everything after it. Deleting a marker cannot restore it from an older response copy.

Older reasoning formats remain readable. Continuing OpenAI conversations that require replay state
stored only in old raw response envelopes is unsupported.

<details>
<summary><b>Testing</b></summary>

- All four providers ignore stale message-level metadata, including when content is emptied.
- Edited tool calls cannot replay stale arguments; opaque items retain their order.
- Compaction keeps the marked turn and never restores deleted markers from old response copies.

</details>

<details>
<summary><b>E2E Testing</b></summary>

Recorded before this rebase: an OpenAI compaction smoke test reduced 15 messages to 2 replay items
and correctly recalled all six earlier values using the stored compaction block.

</details>

<details>
<summary><b>PR Stack</b></summary>

This is PR 6 of 7 in a stack:

1. #2789
2. #2790
3. #2791
4. #2792
5. #2793
6. **This PR**
7. #2795

</details>

<details>
<summary><b>Stack Summary</b></summary>

_Inference providers can share an API format while requiring different authentication, computer
tools, and replay behavior. The old design split this logic between protocols and inference
adapters, allowing providers to inherit assumptions specific to Anthropic or OpenAI._

_This stack removes the inference adapter layer and introduces shared Messages and Responses base
classes with concrete provider implementations. Shared code handles request and response
translation; each provider owns its settings and behavior. Model configuration explicitly selects
the protocol and capabilities._

_Conversation replay also uses the current content blocks, so edits affect the next request.
Additional state needed by a provider is stored on the relevant blocks and interpreted by that
provider's implementation._

</details>
```
