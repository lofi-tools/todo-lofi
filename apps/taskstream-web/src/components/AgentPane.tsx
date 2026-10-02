import { For, Show, createSignal } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { Check, Code, Folder, Sparkles, X } from 'web-design-system/icons'
import { useApp } from '../store'

const pane = css({
  display: 'flex',
  flexDirection: 'column',
  flex: '1',
  minH: '0',
  bg: 'canvas',
})

const head = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  flexShrink: '0',
  h: '12',
  px: '4',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
})

const eyebrow = css({
  flex: '1',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const meta = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1.5',
  h: '6',
  px: '2',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  fontSize: '2xs',
  color: 'fg.muted',
})

const iconButton = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '7',
  h: '7',
  borderRadius: 'sm',
  color: 'fg.subtle',
  cursor: 'pointer',
  _hover: { color: 'fg.default', bg: 'surface.hover' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const transcript = css({
  flex: '1',
  minH: '0',
  overflowY: 'auto',
  px: '4',
  py: '4',
  display: 'flex',
  flexDirection: 'column',
  gap: '3',
})

const bubble = css({
  display: 'flex',
  flexDirection: 'column',
  gap: '1',
  maxW: '92%',
  px: '3',
  py: '2',
  borderRadius: 'md',
  fontSize: 'xs',
  lineHeight: 'relaxed',
  '&[data-role="user"]': {
    alignSelf: 'flex-end',
    bg: 'accent.subtle',
    color: 'fg.default',
  },
  '&[data-role="agent"]': {
    alignSelf: 'flex-start',
    bg: 'surface.subtle',
    color: 'fg.default',
  },
})

const bubbleText = css({ whiteSpace: 'pre-wrap', wordBreak: 'break-word' })

const bubbleTime = css({ fontFamily: 'mono', fontSize: '2xs', color: 'fg.subtle' })

const toolRow = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  alignSelf: 'flex-start',
  maxW: '92%',
  px: '2.5',
  h: '7',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  fontSize: '2xs',
  fontFamily: 'mono',
  color: 'fg.muted',
  '&[data-status="running"]': { borderColor: 'accent', color: 'fg.default' },
  '&[data-status="failed"]': { borderColor: 'danger', color: 'fg.default' },
})

const statusDot = css({
  w: '1.5',
  h: '1.5',
  borderRadius: 'full',
  flexShrink: '0',
  bg: 'fg.subtle',
  '&[data-status="running"]': { bg: 'accent' },
  '&[data-status="done"]': { bg: 'success' },
  '&[data-status="failed"]': { bg: 'danger' },
})

const composer = css({
  display: 'flex',
  flexDirection: 'column',
  gap: '2',
  flexShrink: '0',
  px: '4',
  py: '3',
  borderTopWidth: 'hairline',
  borderTopStyle: 'solid',
  borderTopColor: 'border.subtle',
})

const composerRow = css({ display: 'flex', alignItems: 'flex-end', gap: '2' })

const prompt = css({
  flex: '1',
  minH: '16',
  p: '2',
  borderRadius: 'md',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  bg: 'surface.subtle',
  color: 'fg.default',
  fontSize: 'xs',
  lineHeight: 'relaxed',
  resize: 'vertical',
  _focusVisible: { outline: 'none', borderColor: 'border.strong' },
  _placeholder: { color: 'fg.subtle' },
})

const send = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1.5',
  h: '7',
  px: '3',
  borderRadius: 'sm',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'accent',
  bg: 'accent',
  color: 'fg.onAccent',
  fontSize: 'xs',
  cursor: 'pointer',
  _hover: { opacity: '0.9' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-disabled]': { opacity: '0.5', cursor: 'default' },
})

const composerFoot = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
})

const hint = css({
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1',
  h: '5',
  px: '1.5',
  borderRadius: 'xs',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border.subtle',
  color: 'fg.subtle',
})

interface Message {
  id: number
  role: 'user' | 'agent'
  text: string
  time: string
}

interface ToolCall {
  id: number
  name: string
  detail: string
  status: 'running' | 'done' | 'failed'
}

const PROJECT = 'todo-list'
const AGENT = 'opencode'

let messageId = 100
let toolId = 100

/**
 * The agent pane: one session per directory-backed project. The web version
 * keeps the transcript in the component; the desktop streams it from the ACP
 * connection.
 */
export default function AgentPane() {
  const app = useApp()
  const [messages, setMessages] = createSignal<Message[]>([
    {
      id: 1,
      role: 'user',
      text: 'Task #12: Ship the release\n\nCut the tag, publish the notes, then announce it in the channels.',
      time: '10:04',
    },
    {
      id: 2,
      role: 'agent',
      text: "I'll read the changelog, draft the release notes, and cut the tag. Two things need your call first:\n\n1. Should the tag be `v0.9.0` or `v1.0.0`?\n2. Is the announcement only in #releases, or also the mailing list?",
      time: '10:04',
    },
  ])
  const [tools, setTools] = createSignal<ToolCall[]>([
    { id: 1, name: 'read', detail: 'CHANGELOG.md', status: 'done' },
    { id: 2, name: 'grep', detail: '"release-notes" src/**', status: 'done' },
    { id: 3, name: 'edit', detail: 'docs/release-notes.md', status: 'running' },
  ])
  const [draft, setDraft] = createSignal('')
  const [busy, setBusy] = createSignal(true)
  const [queue, setQueue] = createSignal(0)

  const attachTask = () => {
    const task = app.selectedTask()
    if (!task) return
    const tags = app
      .tagsFor(task)
      .map((tag) => tag.name)
      .join(', ')
    const context = [
      `Task #${task.id}: ${task.title}`,
      task.notes.trim() ? `\n\n${task.notes.trim()}` : '',
      tags ? `\n\nTags: ${tags}` : '',
    ].join('')
    setDraft((current) => (current.trim() ? `${current.trimEnd()}\n\n${context}` : context))
  }

  const submit = () => {
    const text = draft().trim()
    if (!text) return
    // A turn already streaming takes the prompt into its queue, the way the
    // desktop's send does; otherwise it starts a new one.
    if (busy()) {
      setQueue((count) => count + 1)
      setDraft('')
      return
    }
    setMessages((list) => [...list, { id: messageId++, role: 'user', text, time: 'now' }])
    setDraft('')
    setBusy(true)
  }

  const stop = () => {
    setBusy(false)
    setQueue(0)
  }

  return (
    <section class={pane} aria-label="Agent">
      <header class={head}>
        <span class={meta}>
          <Folder size={11} />
          {PROJECT}
        </span>
        <span class={eyebrow}>
          {busy() ? `${AGENT} · running` : AGENT}
          <Show when={queue() > 0}> · {queue()} queued</Show>
        </span>
        <Show when={busy()}>
          <button type="button" class={iconButton} aria-label="Stop the turn" onClick={stop}>
            <X size={13} />
          </button>
        </Show>
      </header>

      <div class={transcript} role="log" aria-label="Transcript">
        <For each={messages()}>
          {(message) => (
            <div class={bubble} data-role={message.role}>
              <span class={bubbleText}>{message.text}</span>
              <span class={bubbleTime}>{message.time}</span>
            </div>
          )}
        </For>

        <For each={tools()}>
          {(tool) => (
            <span class={toolRow} data-status={tool.status}>
              <span class={statusDot} data-status={tool.status} />
              <Code size={11} />
              {tool.name} {tool.detail}
              <Show when={tool.status === 'done'}>
                <Check size={11} />
              </Show>
            </span>
          )}
        </For>
      </div>

      <div class={composer}>
        <div class={composerRow}>
          <textarea
            class={prompt}
            value={draft()}
            placeholder="Ask the agent to do something…"
            aria-label="Prompt"
            onInput={(event) => setDraft(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key !== 'Enter' || !event.metaKey) return
              event.preventDefault()
              submit()
            }}
          />
          <button type="button" class={send} data-disabled={draft().trim() ? undefined : ''} onClick={submit}>
            <Sparkles size={12} />
            {queue() > 0 || busy() ? 'Queue' : 'Send'}
          </button>
        </div>

        <div class={composerFoot}>
          <button type="button" class={cx(hint)} onClick={attachTask}>
            <Sparkles size={10} />
            Attach task
          </button>
          <span>⌘↵ to send</span>
          <span class={css({ flex: '1' })} />
          <span>{busy() ? 'Esc twice to stop' : 'Idle'}</span>
        </div>
      </div>
    </section>
  )
}
