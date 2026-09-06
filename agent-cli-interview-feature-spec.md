# agent-cli: `/interview` feature spec

## 1. Intent

Add a `/interview` slash command that runs an interview flow for an arbitrary user request and, when complete, writes a detailed `*-spec.md` file capturing everything learned.

The interview flow is prompt-driven: `/interview` builds an interview prompt that instructs the active agent to gather context, then ask clarifying questions in rounds using an `ask_user` tool, and finally write a spec file. `/interview` itself is not a separate agent or backend; it is an orchestrated prompt + command state with optional ACP-visible session signaling.

## 2. Scope

### 2.1 Supported entry points

- **TUI slash command**
  - Interactively typed in the TUI as `/interview ...`
  - Primary user-facing path.
- **ACP server**
  - Exposed through the ACP slash-commands extension (not a new base ACP method).
  - ACP clients send `/interview` as a slash command per the ACP slash-commands contract.
- **Single-shot (`-p`) mode**
  - Not in scope for the initial implementation.

### 2.2 Targeting model

- The interview target is a **free-form user request/task**.
- There is no requirement to target a specific named command.
- `/interview` may be used with or without an explicit target argument.

## 3. Syntax and modes

### 3.1 Inline mode

- `/interview <request>` builds the interview prompt immediately using `<request>` as the target.
- Example: `/interview add OAuth support`

### 3.2 Input-mode fallback

- `/interview` with no arguments enters an interview input mode.
- The next user message is treated as the interview target.
- After the target is submitted, the interview prompt is built and sent the same way as inline mode.

### 3.3 Empty target handling

- Empty or whitespace-only targets should be handled gracefully rather than crashing.
- The initial behavior should reject empty targets with a clear message in both TUI and ACP.
- **Error wording is shared across TUI and ACP** for the initial version.
- Suggested shared wording: "Nothing to interview — give /interview a request to clarify."

## 4. Behavior

### 4.1 Prompt building

- One shared interview prompt is used for all models and providers.
- The prompt is built from a fixed `INTERVIEW_BASE_PROMPT` plus the user's raw request text.
- The prompt should instruct the agent to:
  - gather relevant context first
  - ask non-obvious clarifying questions in multiple rounds using the `ask_user` tool
  - direct all questions through `ask_user`, not as plain text
  - ask at least a few rounds of questions when needed
  - write a spec file with everything learned when done

### 4.2 Command semantics

- `/interview` is treated as a **server-side command with extra semantics**, not just a prompt substituted into a normal user message.
- Concretely:
  - The command is recognized and intercepted by the command layer.
  - It transitions the session/command state into an interview flow.
  - The interview prompt is sent to the agent for the current model/provider.
  - The result of the flow is a written spec file and (in TUI) a final confirmation/output.

### 4.3 Spec file output

- Default output path: `./docs/spec/<name>-spec.md`
- No path override mechanism for the initial version.
- The interview prompt should instruct the agent to produce a detailed spec file, but the exact file name/slug is agent-derived from the request.
- **Spec file naming rule:** derive the file name from the request as a slug, for example `<slug>-spec.md`.
- If the request does not produce an obvious slug, the agent should fall back to a sensible name in the same `docs/spec/` location.

### 4.4 Final reply shape

- The interview flow should end by writing the spec file.
- **Final reply behavior:** the final reply should always include a short summary plus the spec file path.
- The summary is included even if the interview only produced a spec file, as long as the reply is sent.
- The important outcome is still the written spec file.

## 5. TUI behavior

### 5.1 Command selection

- `/interview` should appear in the fuzzy `/` command selector alongside the other slash commands.
- Discovery is via the existing fuzzy command selector, not a separate dialog.

### 5.2 Interview input mode

- When `/interview` is submitted with no args, the TUI enters interview input mode.
- The next submitted message becomes the interview target.
- After that, the interview prompt is sent and normal streaming resumes.

### 5.3 Answering `ask_user` questions

- During an interview run, `ask_user` questions should be surfaced through a **separate prompt path** in the TUI.
- While a question is pending, the **main input box is replaced** by a dedicated answer prompt.
- The normal input box is disabled while an ask_user question is pending.
- Answers are submitted through the dedicated answer prompt, not as ordinary chat messages typed into the main input.

### 5.4 `ask_user` block rendering

- Once answered, `ask_user` tool results should render as a **compact question/answer summary block** in the conversation.
- The block should show each question with its chosen or typed answer, or `Skipped` when nothing was provided.
- Expandable/deep option-level rendering is not required for the initial version.

## 6. `ask_user` tool contract

### 6.1 What `ask_user` is

- `ask_user` is the tool used by the interviewing agent to pose clarifying questions.
- `/interview` does not implement question-asking directly; it relies on the agent calling `ask_user` repeatedly via the interview prompt.

### 6.2 Tool registration

- `ask_user` is a **first-class custom tool** in agent-cli's tool set.
- It has a stable tool name and schema.
- It should be registered with the agent so the interviewing agent can call it during an interview.

### 6.3 Schema reference

The tool schema should match the freebuff `AskUserParams` shape:

- `questions`: array of question objects
- Each question has:
  - `question`: string
  - `header?`: short label, ≤12 chars
  - `options?`: `{ label, description? }[]`
  - `multiSelect?`: boolean
  - `validation?`: optional free-text constraints (`maxLength`, `minLength`, `pattern`, `patternError`)

Question shapes:

- single-select (radio) when `multiSelect` is false/omitted
- multi-select (checkbox) when `multiSelect: true`
- free-text “Other” always available, validated if `validation` is present

### 6.4 Answer shape

Answers should support:

- `selectedOption` for single-select matches
- `selectedOptions` for multi-select matches
- `otherText` for free-text/custom input
- a skipped state when nothing is provided

### 6.5 ACP client support

- `ask_user` is surfaced to ACP as a tool the client may need to handle.
- ACP clients that want to participate in interviews must support answering `ask_user` tool calls.
- The ACP interview lifecycle notifications (`interview/started`, `interview/question`, `interview/completed`) are separate from the tool-result flow, but the actual question content should be consistent with the `ask_user` schema.

### 6.6 Rendering

- In the TUI, answered `ask_user` tool results should be rendered as a readable question/answer block.
- Skipped answers should be representable as “Skipped”.
- The tool→block transformation should preserve each question with its chosen or typed answer.

## 7. ACP contract

### 7.1 How ACP invokes `/interview`

- ACP clients invoke `/interview` via the **ACP slash-commands extension**, not a custom ACP method.
- The server recognizes `/interview` as a slash command and runs the interview flow.

### 7.2 ACP-visible interview session

- The interview has **ACP-visible session semantics**, not just an invisible prompt rewrite.
- ACP notifications are **dedicated interview notifications**, not folded into a generic `session/update` interview type.
- The initial implementation exposes a **full interview lifecycle**:
  - `interview/started`
  - `interview/question`
  - `interview/completed`

#### 7.2.1 `interview/started`

- Sent when the interview flow begins.
- Payload should include at least:
  - interview identifier
  - interview target/request being interviewed
  - the prompt/base prompt reference used to start the interview

#### 7.2.2 `interview/question`

- Sent for each round of clarifying questions.
- Payload uses the same `ask_user` question schema as the tool contract.
- A question notification represents one ask-user prompt from the agent to the client.
- Multiple questions may be sent in one round; each should be addressable by the client.

#### 7.2.3 `interview/completed`

- Sent when the interview finishes.
- Payload includes:
  - interview status: success or failure
  - spec file path
  - short summary of what was produced
  - agent's final reply text if any
- Even when no spec file is written, the completion notification should still indicate the outcome.

### 7.3 `ask_user` bridging in ACP

- `ask_user` is exposed as an **agent tool**.
- Interview questions arrive through the agent's normal tool-call flow; the interview lifecycle is surfaced separately via `interview/*` notifications.
- ACP clients must support `ask_user` to fully participate in the interview flow.
- For the initial implementation, `ask_user` bridging is an agent-tool + client-support contract; the TUI implements the local prompt path, while ACP clients implement their own ask-user handling.

## 8. Non-goals / out of scope

- No dedicated “interview agent”.
- No new base ACP method invented for `/interview`.
- No single-shot `-p` support in the initial version.
- No path override for spec output in the initial version.
- No model-specific interview prompt variants in the initial version.

## 9. Open questions for implementation

- Exact TUI answer-prompt UI details: how the dedicated answer prompt is rendered and how keyboard focus/navigation works while the main input is disabled.
- Exact `ask_user` tool registration point and schema version in agent-cli's tool set.
- Whether ACP clients should support `ask_user` as a mandatory tool for interview participation or as an optional capability.
- Final fallback naming rules when the request does not yield an obvious slug for `docs/spec/<name>-spec.md`.

## 10. Notes

- `/interview` follows the same structural pattern as other command flows: register command → optionally build prompt directly or enter input mode → route the submitted target into an interview prompt.
- The interview prompt is the main “logic”; the command layer mainly provides routing, state, and ACP/TUI integration.
- The ask_user tool is the only real question-asking mechanism; `/interview` is a prompt + orchestration wrapper over whatever agent is currently active.
