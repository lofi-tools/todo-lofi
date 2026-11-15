import { For, Show, createEffect, createSignal, createUniqueId, on, onCleanup, type JSX } from 'solid-js'
import { fuzzyScore } from './fuzzy'

export interface PickerOption {
  id: string
  name: string
  detail?: string
  description?: string
  disabled?: boolean
  icon?: JSX.Element
}

export interface PickerGroup {
  label?: string
  value?: string
  options: PickerOption[]
}

export interface PickerMenuProps {
  label: string
  text?: string
  value?: string
  options?: PickerOption[]
  groups?: PickerGroup[]
  onChange?: (value: string) => void
  filterable?: boolean
  disabled?: boolean
}

export function PickerCaret() {
  return (
    <svg class="picker-caret" width="0.45em" height="0.45em" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="m2.5 4.5 3.5 3.5 3.5-3.5" />
    </svg>
  )
}

export function PickerMenu(props: PickerMenuProps & { triggerClass: string; above?: boolean; openRequest?: number; align?: 'start' | 'end'; children: JSX.Element }) {
  const menuId = createUniqueId()
  const [open, setOpen] = createSignal(false)
  const [filterQuery, setFilterQuery] = createSignal('')
  const [activeId, setActiveId] = createSignal<string | null>(null)
  const triggerLabel = () => props.text ? `${props.label}: ${props.text}` : props.label
  let trigger!: HTMLButtonElement
  let menu!: HTMLSpanElement
  let filter!: HTMLInputElement
  createEffect(on(() => props.openRequest, (request) => {
    if (request && !props.disabled && !open()) trigger.click()
  }, { defer: true }))
  const sourceGroups = (): PickerGroup[] => props.groups ?? [{ value: props.value, options: props.options ?? [] }]
  const visibleGroups = () => {
    const query = filterQuery().trim()
    if (!props.filterable || !query) return sourceGroups()
    return sourceGroups()
      .map((group) => ({
        ...group,
        options: group.options
          .map((option) => ({ option, score: fuzzyScore(query, `${option.name} ${option.detail ?? ''}`) }))
          .filter((entry): entry is { option: PickerOption; score: number } => entry.score !== null)
          .sort((left, right) => right.score - left.score)
          .map((entry) => entry.option),
      }))
      .filter((group) => group.options.length > 0)
  }
  const choices = () => visibleGroups().flatMap((group) =>
    group.options.filter((option) => !option.disabled).map((option) => ({ group, option })))

  function positionMenu() {
    const bounds = trigger.getBoundingClientRect()
    const viewportInset = 12
    const gap = 6
    const below = window.innerHeight - bounds.bottom - gap - viewportInset
    const above = bounds.top - gap - viewportInset
    menu.style.removeProperty('--picker-menu-room')
    const opensAbove = props.above ? menu.offsetHeight <= above || above >= below : menu.offsetHeight > below && above > below
    menu.style.setProperty('--picker-menu-room', `${Math.max(0, opensAbove ? above : below)}px`)
    const preferredLeft = props.align === 'end' ? bounds.right - menu.offsetWidth : bounds.left
    const left = Math.min(preferredLeft, window.innerWidth - menu.offsetWidth - viewportInset)
    menu.style.left = `${Math.max(viewportInset, left)}px`
    menu.style.top = `${opensAbove ? bounds.top - gap - menu.offsetHeight : bounds.bottom + gap}px`
  }

  function closeMenu(restoreFocus = true) {
    menu.hidePopover()
    if (restoreFocus) trigger.focus({ preventScroll: true })
    else queueMicrotask(() => { if (document.activeElement === trigger) trigger.blur() })
  }

  function chooseOption(group: PickerGroup, option: PickerOption, restoreFocus = true) {
    closeMenu(restoreFocus)
    if (option.id !== group.value) props.onChange?.(option.id)
  }

  createEffect(() => {
    if (!props.filterable || !open()) return
    const available = choices()
    if (available.some((choice) => choice.option.id === activeId())) return
    const checked = available.find((choice) => choice.group.value === choice.option.id)
    setActiveId(checked?.option.id ?? available[0]?.option.id ?? null)
  })

  createEffect(() => {
    if (!open()) return
    const transcript = trigger.closest('.chat__transcript')
    const dismiss = () => menu.hidePopover()
    transcript?.addEventListener('scroll', dismiss)
    window.addEventListener('resize', positionMenu)
    onCleanup(() => {
      transcript?.removeEventListener('scroll', dismiss)
      window.removeEventListener('resize', positionMenu)
    })
  })

  return (
    <>
      <button
        ref={trigger}
        class={props.triggerClass}
        type="button"
        disabled={props.disabled}
        aria-label={triggerLabel()}
        title={props.above ? triggerLabel() : undefined}
        aria-haspopup="menu"
        aria-controls={menuId}
        aria-expanded={open()}
        popovertarget={menuId}
      >
        {props.children}
      </button>
      <span
        ref={menu}
        id={menuId}
        class="picker-menu"
        popover="auto"
        role="menu"
        tabindex={-1}
        aria-label={props.label}
        on:toggle={(event) => {
          const isOpen = event.newState === 'open'
          setOpen(isOpen)
          if (!isOpen) {
            setFilterQuery('')
            setActiveId(null)
            return
          }
          positionMenu()
          if (props.filterable) {
            filter?.focus({ preventScroll: true })
            return
          }
          const current = menu.querySelector<HTMLButtonElement>('button[aria-checked="true"]:enabled')
            ?? menu.querySelector<HTMLButtonElement>('button:enabled')
            ?? menu
          current.focus()
        }}
        onKeyDown={(event) => {
          if (event.key === 'Tab') {
            closeMenu()
            return
          }
          if (event.key === 'Escape') {
            event.preventDefault()
            closeMenu()
            return
          }
          if (props.filterable) {
            const available = choices()
            if (event.key === 'Enter') {
              event.preventDefault()
              const choice = available.find((candidate) => candidate.option.id === activeId()) ?? available[0]
              if (choice) chooseOption(choice.group, choice.option)
              return
            }
            if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key) || available.length === 0) return
            event.preventDefault()
            const current = available.findIndex((candidate) => candidate.option.id === activeId())
            const step = event.key === 'ArrowUp' ? -1 : 1
            const next = event.key === 'Home'
              ? 0
              : event.key === 'End'
                ? available.length - 1
                : current < 0
                  ? (step > 0 ? 0 : available.length - 1)
                  : (current + step + available.length) % available.length
            setActiveId(available[next]?.option.id ?? null)
            return
          }
          if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
          event.preventDefault()
          const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('button:enabled'))
          if (!items.length) return
          const current = items.findIndex((item) => item === document.activeElement)
          const step = event.key === 'ArrowUp' ? -1 : 1
          let next = (current + step + items.length) % items.length
          if (event.key === 'Home') next = 0
          else if (event.key === 'End') next = items.length - 1
          items[next]?.focus()
        }}
      >
        <Show when={props.filterable}>
          <span class="picker-menu__search">
            <input
              ref={filter}
              class="picker-menu__filter"
              type="text"
              role="searchbox"
              aria-label={`Filter ${props.label.toLowerCase()}`}
              aria-controls={menuId}
              autocomplete="off"
              spellcheck={false}
              placeholder="Filter models"
              value={filterQuery()}
              onInput={(event) => {
                if (event.isComposing) return
                setFilterQuery(event.currentTarget.value)
                setActiveId(null)
              }}
              onCompositionEnd={(event) => {
                setFilterQuery(event.currentTarget.value)
                setActiveId(null)
              }}
            />
          </span>
          <Show when={choices().length === 0}>
            <p class="picker-menu__empty" role="status">No matches.</p>
          </Show>
        </Show>
        <For each={visibleGroups()}>{(group) =>
          <span class="picker-menu__group" role={group.label ? 'group' : undefined} aria-label={group.label}>
            <Show when={group.label}><span class="picker-menu__label" aria-hidden="true">{group.label}</span></Show>
            <For each={group.options}>{(option) => (
              <button
                type="button"
                class="nav-item"
                role={group.value === undefined ? 'menuitem' : 'menuitemradio'}
                tabindex={-1}
                aria-checked={group.value === undefined ? undefined : group.value === option.id}
                title={option.detail}
                disabled={option.disabled}
                classList={{ 'picker-menu__option--active': props.filterable && activeId() === option.id }}
                onClick={(event) => chooseOption(group, option, event.detail === 0)}
              >
                <Show when={option.icon}>
                  <span class="picker-menu__icon">{option.icon}</span>
                </Show>
                <span>{option.name}</span>
                <Show when={option.description}><span class="picker-menu__description">{option.description}</span></Show>
                <Show when={group.value === option.id}>
                  <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="m3.5 8 3 3 6-6" />
                  </svg>
                </Show>
              </button>
            )}</For>
          </span>
        }</For>
      </span>
    </>
  )
}

