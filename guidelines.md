# tabpilot Guidelines

## Goal

Build Tabpilot, an editor-integrated AI completion system that can feel like Cursor Tab while staying provider-neutral.

The first milestone is a fast inline-completion server. It should work with Codex/OpenAI first, but the architecture must not make Codex a hard dependency.

## Product Shape

The system has two different paths:

1. Fast Tab path
   - Runs continuously while editing.
   - Predicts the next local edit near the cursor.
   - Returns ghost text or a small structured edit quickly enough to accept with `<Tab>`.
   - Avoids shell commands, tests, broad agent loops, and large context collection.

2. Agent path
   - Handles explicit, slower workflows: multi-file edits, refactors, reviews, tests, explanations, and patch previews.
   - Can use Codex app-server, Codex SDK, Codex CLI, or another agent backend.
   - Should not block or contaminate the fast completion path.

The server should prefetch. Waiting for the user to press `<Tab>` before asking the model will usually feel slow. The editor should ask for suggestions on idle, text changes, and cursor movement, then let `<Tab>` accept a ready suggestion.

## Protocol

Use Language Server Protocol as the default public editor protocol.

The relevant standard is `textDocument/inlineCompletion` from LSP 3.18. Neovim 0.12 exposes `vim.lsp.inline_completion`, and VS Code can connect through `vscode-languageclient`, so a language server can integrate with native ghost text without a custom editor-only protocol.

Cursor Tab itself does not appear to publish a public protocol. Treat Cursor as product inspiration, not as a protocol dependency.

GitHub Copilot Language Server is a useful precedent:

- Ghost text uses LSP-style inline completion.
- More advanced "next edit" behavior uses custom methods.

That implies this project should start with standard inline completion, then add a private extension later for non-cursor edits, deletions, jump suggestions, and patch previews.

## Proposed Architecture

```text
editor clients
  Neovim Lua plugin
  VS Code extension
  -> starts tabpilot-lsp
      -> implements LSP textDocument/inlineCompletion
      -> owns debounce, cancellation, stale-result rejection, and session state
      -> context engine
          active buffer
          open buffers
          recent edits
          diagnostics
          git diff
          tree-sitter chunks
          LSP symbols, definitions, references
          lexical and embedding retrieval
          project instructions
      -> provider adapters
          OpenAI / Codex
          Ollama
          LM Studio
          vLLM
          LiteLLM
          OpenAI-compatible APIs
          Mistral FIM
          Anthropic-compatible APIs
      -> optional private RPC
          tabpilot/nextEdit
          tabpilot/applyPatchPreview
          tabpilot/explainSuggestion
```

## Provider Abstraction

Do not encode Codex concepts into the core completion engine.

Use an internal provider interface shaped around completion, not around one vendor's API:

```ts
interface CompletionProvider {
  complete(input: CompletionRequest): Promise<CompletionCandidate[]>;
}
```

Provider adapters should translate this internal request into each backend's API shape.

Providers may return multiple structured Completion Candidates with optional Selection Scores in a single call. The initial pipeline should use those scores plus local validation and simple ranking rules. Do not introduce a separate scorer abstraction until provider-returned scores and local heuristics are not enough.

The first adapter can target OpenAI/Codex, but the next likely adapters are:

- OpenAI-compatible HTTP endpoints
- Ollama
- LM Studio
- vLLM
- LiteLLM
- Mistral FIM models
- Tabby or other self-hosted completion services

## Context Model

Context collection must be an explicit subsystem. It should rank and budget context rather than dumping arbitrary text into the prompt.

The first provider path should use a prebuilt `CompletionInput`, not an interactive Context Tool loop. Keep the provider boundary compatible with later Context Tools so adding tool-assisted context does not require a large redesign.

The v1 `CompletionInput` should be viewport-first. It should include active document metadata, cursor position, the visible viewport, and local prefix/suffix context. Do not send the whole document by default; add larger file context later as explicit budgeted context entries.

For v1 Candidate Selection, hard-reject any Completion Candidate whose FileEdits are not fully inside the Visible Viewport. Off-viewport same-file edits can be reconsidered only after Tabpilot has a richer preview workflow.

Candidate shape:

```ts
type ContextEntry = {
  kind:
    | "active_buffer"
    | "open_buffer"
    | "git_diff"
    | "diagnostic"
    | "symbol"
    | "definition"
    | "reference"
    | "retrieved_chunk"
    | "recent_edit"
    | "project_instruction";
  path?: string;
  range?: { startLine: number; endLine: number };
  text: string;
  score: number;
  tokens: number;
  source: string;
};
```

Important context sources:

- Current buffer prefix and suffix around the cursor.
- Current file imports, symbols, and nearby syntax scope.
- Open buffers and recently edited files.
- Recent accepted/rejected suggestions.
- LSP diagnostics.
- LSP definitions, references, and symbols.
- Git diff and staged changes.
- Project instructions such as `AGENTS.md`.
- Tree-sitter chunks.
- Lexical search using ripgrep/BM25.
- Optional embeddings for slower, deeper retrieval.

For latency, the fast path should prefer:

- Active buffer.
- Nearby syntax scope.
- Recent edits.
- Open buffers.
- Diagnostics.
- Cached retrieval results.

Embeddings and whole-repo retrieval are useful, but should be maintained in the background.

## Latency Rules

The completion path has to be engineered around latency from the start.

Core rules:

- Keep a long-running server process.
- Avoid spawning a model process or CLI command per keystroke.
- Debounce editor events.
- Cancel obsolete requests.
- Reject stale results if the buffer version changed.
- Cache stable context prefixes.
- Put static prompt/context before dynamic cursor state when using providers with prompt caching.
- Keep output short.
- Prefer fill-in-the-middle or structured edit formats when the model supports them.
- Do not run broad agent loops in the hot path.

The local server removes startup and context-building overhead. It does not remove remote model inference latency, so prefetch and cancellation are mandatory.

## Codex Surfaces

Codex can be used, but the completion server should not be a thin Codex CLI wrapper.

Observed local Codex surfaces:

- `codex exec`: non-interactive CLI execution.
- `codex mcp-server`: Codex as an MCP server over stdio.
- `codex app-server`: experimental JSON-RPC app server over stdio, Unix socket, or WebSocket.

The Codex app-server is useful for richer agent integration:

- Authentication.
- Conversation/thread state.
- Streamed agent events.
- Diffs and file changes.
- Multi-file agent tasks.

The first real provider should be `CodexProvider`, implemented in `providers/codex.rs`, because the app-server gives Tabpilot a path to Codex authentication, threads, conversation state, and future tool-style context.

Keep that decision behind the provider interface:

- `CompletionEngine` must stay provider-neutral.
- `CodexProvider` translates `CompletionInput` into a Codex app-server turn.
- `CodexProvider` parses structured Codex suggestions back into `CompletionCandidate`s.
- V1 should send a new `turn/start` for every completion request.
- V1 should not use `turn/steer` for inline completions.
- The fast path must not spawn `codex app-server` or a CLI command per keystroke.
- Static prompt instructions should be placed before dynamic cursor context to improve prompt-cache reuse.
- Tabpilot's Codex provider defaults should be local to Tabpilot: `model = "gpt-5.3-codex-spark"` and `model_reasoning_effort = "xhigh"` in app-server requests. Do not edit the user's global Codex config for these defaults, and do not force a `service_tier` override by default.
- `TABPILOT_CODEX_DRY_RUN=1` should force local fake suggestions for provider development without making live model calls.

The app-server does not expose a dedicated keystroke autocomplete endpoint in the inspected schema. Treat the Codex provider as an adapter over Codex threads and turns, and keep the output schema completion-specific and small.

## Existing Projects To Study

- `lsp-ai`: AI language server with editor integration, chat/actions/completion concepts, and multiple model backends.
- `Tabby`: self-hosted AI coding assistant and Copilot-style backend.
- `Continue`: strong reference for provider/model-role abstraction and codebase retrieval strategy.
- GitHub Copilot Language Server release: useful public reference for LSP inline completion plus custom next-edit extensions.

These are architecture references, not dependencies by default.

## Retrieval And Indexing References

Useful building blocks:

- Tree-sitter for incremental syntax parsing and robust syntax-aware chunks.
- SCIP for language-agnostic code intelligence indexes.
- Ripgrep/BM25 for fast lexical retrieval.
- Embeddings/vector search for slower semantic recall.

The likely retrieval plan:

1. Start with active buffer, open buffers, recent edits, diagnostics, and git diff.
2. Add tree-sitter chunking for the current file and nearby files.
3. Add lexical retrieval over the repo.
4. Add embeddings only after the completion loop and ranking interface are stable.
5. Add SCIP/LSP-backed precise symbol retrieval when needed.

## Editor UX

Editor clients should be boring and native:

- Start/stop the language server automatically.
- Use each editor's native inline completion support.
- Provide a simple `<Tab>` accept path.
- Avoid visual clutter.
- Expose logs and debug commands early.
- Make provider/context latency visible during development.

Suggested developer commands:

- `:TabpilotStatus`
- `:TabpilotRestart`
- `:TabpilotLog`
- `:TabpilotContext`

## Open Work We Own

We need to build:

- Editor client bootstrap.
- LSP server.
- Inline completion handler.
- Provider abstraction and first provider adapter.
- Context collector.
- Context ranking and token budgeting.
- Prompt/output format for inline edits.
- Debounce, cancellation, stale result handling.
- Suggestion cache.
- Logging and trace tooling.
- Evaluation harness for latency and acceptance rate.

We should defer:

- Full multi-file next-edit UI.
- Embedding index.
- SCIP index integration.
- Codex app-server agent UI.
- Cross-file patch preview.
- Complex model routing.

## Workspace Layout

Start with one Rust crate at the repository root, using logical module folders instead of a multi-crate workspace:

```text
Cargo.toml
src/
  main.rs
  completion/
  context/
  documents/
  lsp/
  providers/
  protocol/
  config/
  logging/
lua/
  tabpilot/
vscode/
  src/
docs/
```

The crate builds the `tabpilot-lsp` binary. Provider-neutral completion concepts and LSP transport stay separated by module boundaries, not crate boundaries. Editor clients stay thin.

## Early Milestones

1. Server starts and speaks enough LSP for editor clients to attach.
2. `textDocument/inlineCompletion` returns a deterministic mock suggestion.
3. Neovim and VS Code clients start the server and accept ghost text.
4. Provider interface exists with one real provider.
5. Context collector sends active buffer prefix/suffix and cursor metadata.
6. Completion request has debounce, cancellation, and stale result checks.
7. Add basic open-buffer and git-diff context.
8. Add instrumentation for p50/p95 latency and accepted suggestions.

## Debug Instrumentation

Normal Rust logs go through the editor's LSP process logs because the editor owns
the `tabpilot-lsp` process. Keep those logs high-signal and metadata-focused.

For model behavior analysis, enable the Codex transcript:

```bash
TABPILOT_CODEX_DEBUG=1
```

By default this appends JSONL records to:

```text
~/.local/state/tabpilot/codex-debug.jsonl
```

Override the path with:

```bash
TABPILOT_CODEX_DEBUG_PATH=/tmp/tabpilot-codex-debug.jsonl
```

The transcript can contain active file context, full prompts, and model
responses. Treat it as local development data and do not enable it by default.
Important events:

- `turn_start_request`: full prompt sent to Codex.
- `proposal_delta`: streamed model text as Codex emits it.
- `turn_completed_seen`: raw completed-turn payload, whether or not Tabpilot accepts it.
- `turn_completed`: completed response accepted by Tabpilot's current turn matcher.
- `completion_candidates`: parsed candidates returned to the editor client.
- `app_server_notification_ignored`: raw app-server notification that Tabpilot did not handle.

## Language Decision Criteria

The server language should optimize for:

- LSP implementation quality.
- Low startup overhead.
- Easy async cancellation.
- Good JSON-RPC support.
- Simple packaging from editor clients.
- Provider SDK availability.
- Tree-sitter and ripgrep integration.
- Long-term maintainability.

## Sources

- LSP 3.18 `textDocument/inlineCompletion`: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.18/specification/#textDocument_inlineCompletion
- Neovim 0.12 release notes: https://neovim.io/doc/user/news-0.12/
- VS Code inline completion API: https://code.visualstudio.com/api/references/vscode-api#InlineCompletionItemProvider
- Codex app-server docs: https://developers.openai.com/codex/app-server
- Codex SDK docs: https://developers.openai.com/codex/sdk
- OpenAI latency optimization: https://developers.openai.com/api/docs/guides/latency-optimization
- OpenAI prompt caching: https://developers.openai.com/api/docs/guides/prompt-caching
- OpenAI structured outputs: https://developers.openai.com/api/docs/guides/structured-outputs
- GitHub Copilot Language Server release: https://github.com/github/copilot-language-server-release
- lsp-ai: https://github.com/SilasMarvin/lsp-ai
- Tabby: https://github.com/TabbyML/tabby
- Continue autocomplete docs: https://docs.continue.dev/customize/deep-dives/autocomplete
- Continue model providers: https://docs.continue.dev/customize/model-providers/overview
- Continue custom code RAG: https://docs.continue.dev/guides/custom-code-rag
- Tree-sitter: https://tree-sitter.github.io/tree-sitter/
- SCIP: https://scip-code.org/
- Sourcegraph Cody context article: https://sourcegraph.com/blog/how-cody-understands-your-codebase
- LiteLLM OpenAI-compatible providers: https://docs.litellm.ai/docs/providers/openai_compatible
- Ollama OpenAI compatibility: https://docs.ollama.com/api/openai-compatibility
- vLLM OpenAI-compatible server: https://docs.vllm.ai/en/stable/serving/online_serving/
- LM Studio OpenAI compatibility: https://lmstudio.ai/docs/developer/openai-compat
