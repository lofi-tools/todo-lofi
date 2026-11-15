import { For, Show, createEffect, createSignal, createUniqueId, on, onCleanup } from 'solid-js'

import { cancelFileSearch, reserveFileSearch, searchWorkspaceFiles } from './bridge'
import { reportError } from './diagnostics'
import { fuzzyScore } from './fuzzy'
import type { AgentModes, AgentSkill } from './bridge'

interface ComposerCommand {
  id: string
  name: string
  description: string
  disabled: boolean
}

interface ComposerInputProps {
  value: string
  projectId: string | null
  taskId: string | null
  disabled: boolean
  placeholder: string
  inputRef: (input: HTMLTextAreaElement) => void
  onInput: (text: string, composing: boolean) => void
  onSend: () => void
  modes: AgentModes['options']
  modeLabel?: string
  onRemoveMode: () => void
  onModeChange: (mode: string) => void
  skills: AgentSkill[]
  skillError?: string
  onRetrySkills: () => void
  skillToken: (skill: AgentSkill) => string | null
  fallbackSkills: Array<{ id: string; name: string }>
  onSelectFallbackSkill: (skill: AgentSkill) => void
  onRemoveFallbackSkill: (id: string) => void
  skillAtStart: boolean
  commands: ComposerCommand[]
  onCommand: (command: string) => void
  modeError?: string
  onRetryModes: () => void
}

export function ComposerInput(props: ComposerInputProps) {
  const [caret, setCaret] = createSignal<number>()
  const [focused, setFocused] = createSignal(false)
  const [composing, setComposing] = createSignal(false)
  const [dismissed, setDismissed] = createSignal(false)
  const [files, setFiles] = createSignal<string[]>([])
  const [active, setActive] = createSignal(0)
  const [searching, setSearching] = createSignal(false)
  const [failed, setFailed] = createSignal(false)
  const listId = createUniqueId()
  let input: HTMLTextAreaElement | undefined

  const mention = () => {
    const end = caret()
    if (end === undefined || !focused() || composing() || dismissed() || props.disabled || !props.projectId) return
    if (end < props.value.length && !/\s/u.test(props.value.charAt(end))) return
    const match = /(?:^|[\s([{])@([^\s"@()]*)$/u.exec(props.value.slice(0, end))
    if (match) return { start: end - match[1].length - 1, end, query: match[1] }
  }

  const command = () => {
    const end = caret()
    if (end === undefined || !focused() || composing() || dismissed() || props.disabled || !props.projectId) return
    if (end < props.value.length && !/\s/u.test(props.value.charAt(end))) return
    const match = /(?:^|[\s([{])\/([^\s/]*)$/u.exec(props.value.slice(0, end))
    if (match) return { start: end - match[1].length - 1, end, query: match[1] }
  }
  const matched = <T extends { id: string; name: string; description?: string | null }>(options: T[]) => {
    const query = command()?.query ?? ''
    return options.map((option) => ({ option, score: fuzzyScore(query, `${option.id} ${option.name} ${option.description ?? ''}`) }))
      .filter((entry) => entry.score !== null)
      .sort((left, right) => (right.score ?? 0) - (left.score ?? 0))
      .map((entry) => entry.option)
  }
  const matchingCommands = () => matched(props.commands)
  const matchingModes = () => matched(props.modes)
  const matchingSkills = () => matched(props.skills)
  const selectedSkills = () => props.skills.filter((skill) => {
    const token = props.skillToken(skill)?.trimEnd()
    return !!token && (props.value.includes(`${token} `) || props.value.endsWith(token))
  })
  const choices = () => [
    ...matchingCommands().map((option) => ({ kind: 'command' as const, option })),
    ...matchingModes().map((option) => ({ kind: 'mode' as const, option })),
    ...matchingSkills().map((option) => ({ kind: 'skill' as const, option })),
  ]
  const enabledChoices = () => choices().filter((choice) => choice.kind !== 'command' || !choice.option.disabled)
  const choiceIndex = (choice: ReturnType<typeof choices>[number]) => choices().findIndex((entry) => entry.kind === choice.kind && entry.option.id === choice.option.id)
  createEffect(on(() => command()?.query, () => setActive(0)))
  createEffect(on(() => !!command(), (opened) => { if (opened) props.onRetrySkills() }, { defer: true }))
  createEffect(() => { if (command() && active() >= enabledChoices().length) setActive(0) })

  createEffect(on([() => props.projectId, () => props.taskId, () => mention()?.query], ([projectId, taskId, query]) => {
    setFiles([])
    setActive(0)
    setFailed(false)
    setSearching(query !== undefined)
    if (!projectId || query === undefined) return
    let disposed = false
    let searchId: string | undefined
    const cancel = (id: string) => {
      void cancelFileSearch(id).catch((failure) => console.warn('Could not cancel file mention search', failure))
    }
    const timer = window.setTimeout(async () => {
      try {
        searchId = await reserveFileSearch()
        if (disposed) {
          cancel(searchId)
          return
        }
        const result = await searchWorkspaceFiles(searchId, projectId, taskId, { mode: 'path', query, showDotfiles: false })
        if (!disposed) setFiles(result.matches.slice(0, 12).map((file) => file.path))
      } catch (failure) {
        if (!disposed) { setFailed(true); reportError('search_workspace_files', failure) }
      } finally {
        searchId = undefined
        if (!disposed) setSearching(false)
      }
    }, 80)
    onCleanup(() => {
      disposed = true
      window.clearTimeout(timer)
      if (searchId) cancel(searchId)
    })
  }))

  function refreshCaret(element: HTMLTextAreaElement) {
    setCaret(element.selectionStart === element.selectionEnd ? element.selectionStart : undefined)
  }

  function update(element: HTMLTextAreaElement, isComposing: boolean) {
    setComposing(isComposing)
    setDismissed(false)
    props.onInput(element.value, isComposing)
    refreshCaret(element)
  }

  function choose(path: string) {
    const current = mention()
    if (!current || !input) return
    const token = `@file(${JSON.stringify(path)}) `
    const text = props.value.slice(0, current.start) + token + props.value.slice(current.end).replace(/^ /, '')
    setDismissed(true)
    props.onInput(text, false)
    input.focus()
    input.setSelectionRange(current.start + token.length, current.start + token.length)
    refreshCaret(input)
  }

  function chooseCommand(choice: ReturnType<typeof choices>[number]) {
    const current = command()
    if (!current || !input || (choice.kind === 'command' && choice.option.disabled)) return
    const skillToken = choice.kind === 'skill' ? props.skillToken(choice.option) : null
    const token = skillToken ?? ''
    const before = props.value.slice(0, current.start)
    const after = props.value.slice(current.end).replace(/^ /, '')
    const prefixSkill = choice.kind === 'skill' && props.skillAtStart
    const text = prefixSkill ? token + before + after : before + token + after
    const cursor = prefixSkill ? token.length + before.length : current.start + token.length
    setDismissed(true)
    props.onInput(text, false)
    input.focus()
    input.setSelectionRange(cursor, cursor)
    refreshCaret(input)
    if (choice.kind === 'mode') props.onModeChange(choice.option.id)
    else if (choice.kind === 'skill' && skillToken === null) props.onSelectFallbackSkill(choice.option)
    else if (choice.kind === 'command') props.onCommand(choice.option.id)
  }

  function removeNativeSkill(skill: AgentSkill) {
    const token = props.skillToken(skill)
    if (!token || !input) return
    let index = props.value.indexOf(token)
    if (index < 0) {
      const withoutSpace = token.trimEnd()
      if (!props.value.endsWith(withoutSpace)) return
      index = props.value.length - withoutSpace.length
    }
    let text = props.value
    while (text.includes(token)) text = text.replace(token, '')
    const withoutSpace = token.trimEnd()
    if (text.endsWith(withoutSpace)) text = text.slice(0, -withoutSpace.length)
    props.onInput(text, false)
    input.focus()
    input.setSelectionRange(Math.min(index, text.length), Math.min(index, text.length))
    refreshCaret(input)
  }

  const rowId = (index: number) => `${listId}-${index}`
  function keyDown(event: KeyboardEvent) {
    if (event.defaultPrevented || event.isComposing || composing() || event.keyCode === 229) return
    if (command()) {
      if (event.key === 'Escape') {
        event.preventDefault()
        setDismissed(true)
        return
      }
      const modes = enabledChoices()
      if ((event.key === 'ArrowDown' || event.key === 'ArrowUp') && modes.length) {
        event.preventDefault()
        const step = event.key === 'ArrowDown' ? 1 : -1
        const next = (active() + step + modes.length) % modes.length
        setActive(next)
        document.getElementById(rowId(choiceIndex(modes[next])))?.scrollIntoView({ block: 'nearest' })
        return
      }
      if (event.key === 'Enter' && !event.shiftKey && modes.length) {
        event.preventDefault()
        const mode = modes[active()]
        if (mode) chooseCommand(mode)
        return
      }
    }
    if (mention()) {
      if (event.key === 'Escape') {
        event.preventDefault()
        setDismissed(true)
        return
      }
      if ((event.key === 'ArrowDown' || event.key === 'ArrowUp') && files().length) {
        event.preventDefault()
        const step = event.key === 'ArrowDown' ? 1 : -1
        const next = (active() + step + files().length) % files().length
        setActive(next)
        document.getElementById(rowId(next))?.scrollIntoView({ block: 'nearest' })
        return
      }
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault()
        const file = files()[active()]
        if (file) choose(file)
        return
      }
    }
    if (event.key === 'Enter' && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault()
      props.onSend()
    }
  }

  return (
    <>
      <Show when={props.modeLabel || selectedSkills().length || props.fallbackSkills.length}>
        <div class="composer__selections">
          <Show when={props.modeLabel}>{(mode) => <span class="composer__mode" role="status"><ModeMark /><span>Mode: {mode()}</span><button type="button" class="composer__skill-remove" aria-label={`Remove mode ${mode()}`} disabled={props.disabled} onClick={() => { props.onRemoveMode(); input?.focus() }}>×</button></span>}</Show>
          <Show when={selectedSkills().length || props.fallbackSkills.length}>
        <ul class="composer__skills" aria-label="Selected skills">
          <For each={selectedSkills()}>{(skill) => <li aria-label={`Selected skill: ${skill.name}`}>
            <SkillMark /><span>{skill.name}</span>
            <button type="button" class="composer__skill-remove" aria-label={`Remove skill ${skill.name}`} disabled={props.disabled}
              onClick={() => removeNativeSkill(skill)}>×</button>
          </li>}</For>
          <For each={props.fallbackSkills}>{(skill) => <li aria-label={`Selected skill: ${skill.name}`}>
            <SkillMark /><span>{skill.name}</span>
            <button type="button" class="composer__skill-remove" aria-label={`Remove skill ${skill.name}`} disabled={props.disabled}
              onClick={() => { props.onRemoveFallbackSkill(skill.id); input?.focus() }}>×</button>
          </li>}</For>
        </ul>
          </Show>
        </div>
      </Show>
      <textarea
        ref={(element) => { input = element; props.inputRef(element) }}
        class="composer__input"
        aria-label="Prompt"
        aria-autocomplete="list"
        aria-controls={mention() || command() ? listId : undefined}
        aria-activedescendant={mention() && files().length ? rowId(active()) : command() && enabledChoices().length ? rowId(choiceIndex(enabledChoices()[active()] ?? enabledChoices()[0])) : undefined}
        placeholder={props.placeholder}
        rows={2}
        value={props.value}
        disabled={props.disabled}
        onInput={(event) => update(event.currentTarget, event.isComposing)}
        onCompositionStart={() => setComposing(true)}
        onCompositionEnd={(event) => update(event.currentTarget, false)}
        onFocus={(event) => { setFocused(true); setDismissed(false); refreshCaret(event.currentTarget) }}
        onBlur={() => setFocused(false)}
        onClick={(event) => refreshCaret(event.currentTarget)}
        onSelect={(event) => refreshCaret(event.currentTarget)}
        onKeyUp={(event) => refreshCaret(event.currentTarget)}
        onKeyDown={keyDown}
      />
      <Show when={command()}>
        <div class="composer__mentions composer__commands">
          <Show when={props.modeError}><div class="picker__root" role="status">{props.modeError} <button type="button" onMouseDown={(event) => event.preventDefault()} onClick={props.onRetryModes}>Retry</button></div></Show>
          <Show when={!choices().length}><p class="picker__root" role="status">No commands match.</p></Show>
          <ul class="picker__list" id={listId} role="listbox" aria-label="Composer commands">
            <Show when={matchingCommands().length}><li class="picker__root composer__command-heading" role="presentation">Commands</li></Show>
            <For each={matchingCommands()}>{(option, index) => {
              const choice = () => ({ kind: 'command' as const, option })
              const activeIndex = () => enabledChoices().findIndex((entry) => entry.kind === 'command' && entry.option.id === option.id)
              return <li role="presentation"><button type="button" id={rowId(index())} class="picker__row"
                classList={{ 'picker__row--active': activeIndex() >= 0 && active() === activeIndex() }}
                role="option" disabled={option.disabled} aria-selected={activeIndex() >= 0 && active() === activeIndex()} tabindex={-1}
                onMouseDown={(event) => event.preventDefault()} onClick={() => chooseCommand(choice())}>
                <span class="composer__command-icon" aria-hidden="true">/</span><span class="composer__command-name">{option.name}</span><small class="composer__command-description" title={option.description}>{option.description}</small>
              </button></li>
            }}</For>
            <Show when={matchingModes().length}><li class="picker__root composer__command-heading" role="presentation">Modes</li></Show>
            <For each={matchingModes()}>{(option, index) => {
              const choice = () => ({ kind: 'mode' as const, option })
              const activeIndex = () => enabledChoices().findIndex((entry) => entry.kind === 'mode' && entry.option.id === option.id)
              return <li role="presentation"><button type="button" id={rowId(matchingCommands().length + index())} class="picker__row"
                classList={{ 'picker__row--active': active() === activeIndex() }}
                role="option" aria-selected={active() === activeIndex()} tabindex={-1}
                onMouseDown={(event) => event.preventDefault()} onClick={() => chooseCommand(choice())}>
                <ModeMark /><span class="composer__command-name">{option.name}</span><Show when={option.description}>{(description) => <small class="composer__command-description" title={description()}>{description()}</small>}</Show>
              </button></li>
            }}</For>
            <Show when={props.skillError}><li class="picker__root" role="presentation">Could not load skills: {props.skillError} <button type="button" onMouseDown={(event) => event.preventDefault()} onClick={props.onRetrySkills}>Retry</button></li></Show>
            <Show when={matchingSkills().length}><li class="picker__root composer__command-heading" role="presentation">Skills</li></Show>
            <For each={matchingSkills()}>{(option, index) => {
              const choice = () => ({ kind: 'skill' as const, option })
              const activeIndex = () => enabledChoices().findIndex((entry) => entry.kind === 'skill' && entry.option.id === option.id)
              return <li role="presentation"><button type="button" id={rowId(matchingCommands().length + matchingModes().length + index())} class="picker__row"
                classList={{ 'picker__row--active': active() === activeIndex() }}
                role="option" aria-selected={active() === activeIndex()} tabindex={-1}
                onMouseDown={(event) => event.preventDefault()} onClick={() => chooseCommand(choice())}>
                <SkillMark /><span class="composer__command-name">{option.name}</span><small class="composer__command-description" title={option.description}>{option.description}</small>
              </button></li>
            }}</For>
          </ul>
        </div>
      </Show>
      <Show when={mention()}>
        <div class="composer__mentions">
          <Show when={searching() || (!failed() && !files().length)}>
            <p class="picker__root" role="status">{searching() ? 'Searching workspace files…' : 'No workspace files match.'}</p>
          </Show>
          <ul class="picker__list" id={listId} role="listbox" aria-label="Mention a workspace file" aria-busy={searching()}>
            <For each={files()}>{(path, index) =>
              <li role="presentation">
                <button type="button" id={rowId(index())} class="picker__row"
                  classList={{ 'picker__row--active': active() === index() }}
                  role="option" aria-selected={active() === index()} tabindex={-1}
                  onMouseDown={(event) => event.preventDefault()} onClick={() => choose(path)}>
                  {path}
                </button>
              </li>
            }</For>
          </ul>
        </div>
      </Show>
    </>
  )
}

function SkillMark() {
  return <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m12 3 1.8 5.4L19 10l-5.2 1.7L12 17l-1.8-5.3L5 10l5.2-1.6L12 3Zm6 11 .9 2.7L21.5 18l-2.6.9L18 21.5l-.9-2.6-2.6-.9 2.6-1.3L18 14Z" /></svg>
}

function ModeMark() {
  return <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 7h4m4 0h8M4 17h8m4 0h4" /><circle cx="10" cy="7" r="2" /><circle cx="14" cy="17" r="2" /></svg>
}
