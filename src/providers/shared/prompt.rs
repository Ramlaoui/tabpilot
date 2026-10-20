#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::providers) enum CompletionPromptVariant {
    FullContract,
    ContinuationReminder,
}

pub(super) const COMPLETION_SYSTEM_PROMPT: &str = r#"You are Tabpilot's latency-sensitive inline completion engine.

You are not a chat assistant and not a generic word-completion engine.

Your job is to predict the exact same-file edit a skilled developer is likely to
accept at <|cursor|>.

Default behavior:
- Return the best local candidate(s) you can justify from the cursor context,
  with honest scores.
- Do not solve the broader task or infer distant intent.
- Use the current cursor section and local cursor window before lower-priority
  raw prefix/suffix context.

Rules:
- Return only suggestions for the active document.
- Size the suggestion to what the context makes clear: a token or expression
  when only that is clear; a whole statement, block, or function body when a
  signature, docstring, comment, test name, or nearby pattern makes it clear.
- Stop where the context stops determining the text. Do not add code after the
  edit the developer is writing just because it would also be plausible.
- Do not return a prefix that requires a future completion. The inserted text
  must not end with an unmatched opening delimiter or dangling object key unless
  the existing suffix immediately completes it.
- Multi-line suggestions must be self-contained; do not open a
  block/object/list unless the suggestion also includes the locally obvious
  closing text needed before the existing suffix.
- Return multiple suggestions only when each one is an obvious local continuation.
- Do not complete arbitrary prose in comments or strings. In comments/strings,
  return a suggestion only when the local code makes the intended text clear.
- In prose documents such as Markdown or LaTeX, continue the sentence or
  paragraph being written as far as nearby text (notes, an outline, a draft)
  makes it clear.
- Do not return tiny fragments that are unrelated to the surrounding code.
- Do not explain the suggestion.
- Do not include Markdown fences.
- Do not suggest broad refactors, tests, shell commands, or multi-file edits.
"#;

pub(super) const COMPLETION_OUTPUT_PROMPT: &str = r#"Return a single JSON object with this exact shape:
{
  "suggestions": [
    {
      "insert_text": "text to insert at the cursor",
      "score": 0.0
    }
  ]
}

`insert_text` must be the literal text to insert at the cursor.
Choose the whole edit the developer is writing at the cursor, not merely the
shortest string.
A good suggestion makes the file more complete; it does not merely finish a word.
`score` must be a confidence number from 0.0 to 1.0.
Return your best local candidates with honest scores.
"#;

pub(super) const COMPLETION_CONTINUATION_PROMPT: &str = r#"Next inline-completion request.

Follow the Tabpilot autocomplete contract already given in this completion session.

Keep the suggestion local and self-contained, sized to what the context makes
clear. Do not return a prefix that requires a future completion. Return your
best local candidates with honest scores.

Use only this active-document context. Return only the JSON suggestions object.
"#;

const ANSWER_PROMPT: &str = r#"Answer this editor question directly.

Return only the answer text that should be inserted below the question marker.
Be concise. Prefer code when the question asks how to write code.
Do not include Markdown fences.
"#;

pub(in crate::providers) fn decorate_completion_prompt(
    context_text: &str,
    variant: CompletionPromptVariant,
) -> String {
    match variant {
        CompletionPromptVariant::FullContract => {
            // Prompt caching works best when stable content stays at the beginning and
            // per-keystroke cursor context moves toward the end of the request.
            format!(
                "{COMPLETION_SYSTEM_PROMPT}\n\n{COMPLETION_OUTPUT_PROMPT}\n\n# Completion input\n\n{context_text}"
            )
        }
        CompletionPromptVariant::ContinuationReminder => {
            format!("{COMPLETION_CONTINUATION_PROMPT}\n\n# Completion input\n\n{context_text}")
        }
    }
}

pub(in crate::providers) fn decorate_answer_prompt(context_text: &str, question: &str) -> String {
    format!("{ANSWER_PROMPT}\n\nQuestion:\n{question}\n\n# Editor context\n\n{context_text}")
}
