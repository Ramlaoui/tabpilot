# tabpilot

This context defines the project language for an editor-integrated AI completion system. It keeps product and domain terms distinct from implementation details.

## Language

**Tabpilot**:
The editor-integrated system that provides Tab-driven inline completion and future next-edit assistance. Tabpilot includes editor clients, the Completion Server, and provider integrations.
_Avoid_: Codex Complete, Cursor clone, Copilot clone

**Completion Server**:
The long-running process inside Tabpilot that owns an editor session's completion state and returns inline completion candidates to editor clients. It is separate from editor plugins/extensions and separate from any model provider.
_Avoid_: LSP server when discussing product responsibility, Codex server, model server

**Model Provider**:
The adapter used by the Completion Server to ask a model backend for Completion Candidates. A Model Provider may request Context Tools, but it does not own editor state and does not choose the final Suggestion.
_Avoid_: Model when discussing the adapter boundary, provider-side selector

**Context Tool**:
A read-only capability exposed by the Completion Server to a Model Provider during candidate generation. Context Tools let the provider ask for more editor or code context without directly accessing editor state.
_Avoid_: Agent tool for Tab-driven completion, mutating tool

**Completion Candidate**:
A possible same-file inline edit set produced inside the Completion Server before it is chosen for display. A Completion Candidate contains one or more FileEdits, and most candidates may never be shown to the user.
_Avoid_: Suggestion when discussing unshown server-side options

**Suggestion**:
The Completion Candidate currently displayed to the user as ghost text or another editor-visible affordance. A Suggestion is user-facing, should stay in the visible viewport when possible, and can be accepted, ignored, or replaced.
_Avoid_: Completion Candidate when discussing what the user sees

**FileEdit**:
A range replacement implicitly scoped to the active document. A FileEdit does not carry its own file path; changes to other files belong to an explicit agent workflow, not Tab-driven completion.
_Avoid_: Multi-file edit, cross-file edit, path-bearing edit

**Visible Viewport**:
The currently visible region of the active document in the editor client. Tabpilot should prefer Suggestions whose FileEdits fall inside this region so the user can visually verify them before accepting.
_Avoid_: Whole-file suggestion region

**Candidate Selection**:
The process inside the Completion Server that chooses which Completion Candidate becomes the visible Suggestion. Providers may contribute candidates or scores, but the Completion Server owns the final choice.
_Avoid_: Provider-side selection, UI-side selection

**Selection Score**:
A comparable score used during Candidate Selection. A Selection Score may come from heuristics, a provider response, or a separate ranking call, but it is not treated as a calibrated probability unless explicitly calibrated.
_Avoid_: Probability score unless the value is actually calibrated

## Example Dialogue

Dev: "Does Tabpilot call Codex directly?"

Domain expert: "Tabpilot may use a Codex-backed provider, but the Completion Server should remain provider-neutral."

Dev: "Can the Model Provider inspect more of the file?"

Domain expert: "It can request a Context Tool. The Completion Server decides what context is returned."

Dev: "The provider returned three suggestions. Is that right?"

Domain expert: "Call those Completion Candidates. Only the candidate shown in the editor is a Suggestion."

Dev: "Can a Suggestion update two nearby lines?"

Domain expert: "Yes, if both FileEdits are in the active document and ideally inside the Visible Viewport."

Dev: "The model gave this candidate a probability score. Can we show it?"

Domain expert: "Call it a Selection Score unless we have calibrated probabilities. The Completion Server still decides whether it becomes the Suggestion."
