//! Shared `/interview` command logic: the base prompt and prompt builder
//! used by both the TUI and the ACP server so every entry point runs the
//! same interview flow.

/// Base prompt for every `/interview` flow. The user's raw request text is
/// appended so the same prompt works for all models and providers.
pub const INTERVIEW_BASE_PROMPT: &str = "\
You are running an interview for a user request. Your job is to gather context \
and ask clarifying questions before producing a detailed spec.

## Process

1. First, gather relevant context about the request — read files, search the \
   codebase, check existing docs — whatever helps you understand the current \
   state.
2. Then ask clarifying questions using the `ask_user` tool. Ask at least a \
   few rounds of questions when needed. Always use `ask_user` for questions, \
   never plain text.
3. When you have enough context, write a detailed spec file.

## Spec file output

- Write the spec to `./docs/spec/<slug>-spec.md` where `<slug>` is derived from \
   the request (a short kebab-case name).
- If the request doesn't suggest an obvious slug, use a sensible name in the \
   same `docs/spec/` location.
- The spec should be detailed: capture everything you learned during the \
   interview — requirements, constraints, decisions, open questions, and the \
   planned approach.
- Create the `docs/spec/` directory if it doesn't exist.

## Final reply

- After writing the spec file, reply with a short summary plus the spec file \
   path (e.g. `Wrote spec to ./docs/spec/add-oauth-spec.md`).
- The summary is included even if the interview only produced a spec file.

## Request

Request to interview: ";

/// Shared error wording for empty/whitespace-only interview targets, used
/// identically by the TUI and the ACP server.
pub const EMPTY_TARGET_MESSAGE: &str = "Nothing to interview — give /interview a request to clarify.";

/// Build the full interview prompt from the user's raw request text.
pub fn build_interview_prompt(target: &str) -> String {
    format!("{INTERVIEW_BASE_PROMPT}{target}")
}







// Interview me to better understand my request and then create a spec file. First, gather any relevant context (read files, do research, etc.). Then, use several rounds of the ask_user tool to ask non-obvious clarifying questions — things you cannot easily infer from the codebase or my initial message. Ask about edge cases, preferences, constraints, and design decisions. All questions should be directed through the ask_user tool -- not written out as text. Keep coming up with new questions that get at unique aspects of the request. Aim for at least 3 rounds with multiple questions each round. When satisfied, write a [INSERT_REQUEST_SHORT_NAME]-spec.md file with all the information you have gathered about the request. Aim for as much detail as possible...  
