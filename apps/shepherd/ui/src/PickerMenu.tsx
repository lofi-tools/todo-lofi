import { For, Show, createEffect, createMemo, createSignal, createUniqueId, on, onCleanup, type JSX } from 'solid-js'
import { fuzzyScore } from './fuzzy'

export interface PickerOption {
  id: string
  name: string
  detail?: string
  description?: string
  disabled?: boolean
  icon?: JSX.Element
  keepOpen?: boolean
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
  customOption?: (query: string) => PickerOption | undefined
  disabled?: boolean
  footer?: JSX.Element
}

export function PickerCaret() {
  return (
    <svg class="picker-caret" width="0.45em" height="0.45em" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="m2.5 4.5 3.5 3.5 3.5-3.5" />
    </svg>
  )
}

export function PickerMenu(props: PickerMenuProps & {
  triggerClass: string
  above?: boolean
  openRequest?: number
  align?: 'start' | 'end'
  within?: string
  contextTarget?: () => HTMLElement
  contextOnly?: boolean
  children: JSX.Element
}) {
  const menuId = createUniqueId()
  const footer = createMemo(() => props.footer)
  const [open, setOpen] = createSignal(false)
  const [filterQuery, setFilterQuery] = createSignal('')
  const [activeId, setActiveId] = createSignal<string | null>(null)
  const triggerLabel = () => props.text ? `${props.label}: ${props.text}` : props.label
  let trigger!: HTMLButtonElement
  let menu!: HTMLSpanElement
  let filter!: HTMLInputElement
  let contextAnchor: { target: HTMLElement; x: number; y: number } | undefined
  createEffect(on(() => props.openRequest, (request) => {
    if (request && !props.disabled && !open()) trigger.click()
  }, { defer: true }))
  const sourceGroups = (): PickerGroup[] => props.groups ?? [{ value: props.value, options: props.options ?? [] }]
  const visibleGroups = () => {
    const query = filterQuery().trim()
    if (!props.filterable || !query) return sourceGroups()
    const groups = sourceGroups()
      .map((group) => ({
        ...group,
        options: group.options
          .map((option) => ({ option, score: fuzzyScore(query, `${option.name} ${option.detail ?? ''}`) }))
          .filter((entry): entry is { option: PickerOption; score: number } => entry.score !== null)
          .sort((left, right) => right.score - left.score)
          .map((entry) => entry.option),
      }))
      .filter((group) => group.options.length > 0)
    const custom = props.customOption?.(query)
    return custom ? [...groups, { options: [custom] }] : groups
  }
  const choices = () => visibleGroups().flatMap((group) =>
    group.options.filter((option) => !option.disabled).map((option) => ({ group, option })))

  function positionMenu() {
    const viewportInset = 12
    const gap = 6
    const target = contextAnchor?.target ?? trigger
    const bounds = target.getBoundingClientRect()
    const top = contextAnchor?.y ?? bounds.top
    const bottom = contextAnchor?.y ?? bounds.bottom
    const boundary = props.within ? target.closest(props.within)?.getBoundingClientRect() : undefined
    const leftInset = boundary ? Math.max(viewportInset, boundary.left + gap) : viewportInset
    if (boundary) menu.style.setProperty('--picker-menu-width', `${Math.max(0, bounds.right - leftInset)}px`)
    const below = window.innerHeight - bottom - gap - viewportInset
    const above = top - gap - viewportInset
    menu.style.removeProperty('--picker-menu-room')
    const opensAbove = props.above ? menu.offsetHeight <= above || above >= below : menu.offsetHeight > below && above > below
    menu.style.setProperty('--picker-menu-room', `${Math.max(0, opensAbove ? above : below)}px`)
    const preferredLeft = contextAnchor?.x ?? (props.align === 'end' ? bounds.right - menu.offsetWidth : bounds.left)
    const left = Math.min(preferredLeft, window.innerWidth - menu.offsetWidth - viewportInset)
    menu.style.left = `${Math.max(leftInset, left)}px`
    menu.style.top = `${opensAbove ? top - gap - menu.offsetHeight : bottom + gap}px`
  }

  function closeMenu(restoreFocus = true) {
    const focusTarget = props.contextOnly ? contextAnchor?.target : trigger
    if (menu.matches(':popover-open')) menu.hidePopover()
    if (restoreFocus) focusTarget?.focus({ preventScroll: true })
    else queueMicrotask(() => { if (document.activeElement === focusTarget) focusTarget?.blur() })
  }

  createEffect(() => {
    const target = props.contextTarget?.()
    if (!target) return
    const showContextMenu = (event: MouseEvent) => {
      event.preventDefault()
      if (props.disabled || (event.target instanceof Node && menu.contains(event.target))) return
      const keyboard = event.clientX === 0 && event.clientY === 0
      const bounds = target.getBoundingClientRect()
      contextAnchor = { target, x: keyboard ? bounds.left : event.clientX, y: keyboard ? bounds.bottom : event.clientY }
      if (menu.matches(':popover-open')) positionMenu()
      else menu.showPopover()
    }
    target.addEventListener('contextmenu', showContextMenu)
    onCleanup(() => target.removeEventListener('contextmenu', showContextMenu))
  })

  function chooseOption(group: PickerGroup, option: PickerOption, restoreFocus = true) {
    if (option.id !== group.value) props.onChange?.(option.id)
    if (option.keepOpen) {
      setActiveId(option.id)
      filter?.focus({ preventScroll: true })
    } else {
      closeMenu(restoreFocus)
    }
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
    const scrollPane = (contextAnchor?.target ?? trigger).closest('.chat__transcript, .settings__detail, .terminal-list')
    const dismiss = () => menu.hidePopover()
    scrollPane?.addEventListener('scroll', dismiss)
    const reposition = () => positionMenu()
    const resizeObserver = footer() ? new ResizeObserver(reposition) : undefined
    resizeObserver?.observe(menu)
    window.addEventListener('resize', reposition)
    onCleanup(() => {
      scrollPane?.removeEventListener('scroll', dismiss)
      window.removeEventListener('resize', reposition)
      resizeObserver?.disconnect()
    })
  })

  return (
    <>
      <Show when={!props.contextOnly}><button
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
      </button></Show>
      <span
        ref={menu}
        id={menuId}
        class="picker-menu"
        classList={{ 'picker-menu--footer': !!footer() }}
        popover="auto"
        role="menu"
        tabindex={-1}
        aria-label={props.label}
        on:toggle={(event) => {
          const isOpen = event.newState === 'open'
          setOpen(isOpen)
          if (!isOpen) {
            contextAnchor = undefined
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
        onFocusOut={(event) => {
          if (footer() && open() && event.relatedTarget instanceof Node && !menu.contains(event.relatedTarget)) {
            closeMenu(false)
          }
        }}
        onKeyDown={(event) => {
          if (event.key === 'Tab') {
            if (!footer()) closeMenu()
            return
          }
          if (event.key === 'Escape') {
            event.preventDefault()
            closeMenu()
            return
          }
          if (props.filterable) {
            if (event.target !== filter && footer()) return
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
        </Show>
        <span class="picker-menu__options">
          <Show when={props.filterable && choices().length === 0}>
            <p class="picker-menu__empty" role="status">No matches.</p>
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
        {footer()}
      </span>
    </>
  )
}

