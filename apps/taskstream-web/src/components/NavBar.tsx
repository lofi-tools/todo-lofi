import { For, Show, createSignal, type JSX } from 'solid-js'
import { css, cx } from 'styled-system/css'
import {
  Box,
  Folder,
  Inbox,
  Layers,
  Link as LinkIcon,
  LogoMark,
  Repeat,
  Settings,
  Zap,
} from 'web-design-system/icons'
import { useApp, type Destination } from '../store'

const rail = css({
  display: 'flex',
  flexDirection: 'column',
  w: '60',
  flexShrink: '0',
  minH: '0',
  borderRightWidth: 'hairline',
  borderRightStyle: 'solid',
  borderRightColor: 'border.subtle',
  bg: 'surface.subtle',
})

const brand = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  h: '12',
  px: '4',
  flexShrink: '0',
  fontSize: 'sm',
  fontWeight: 'semibold',
  letterSpacing: 'tight',
  color: 'fg.default',
})

const scroll = css({
  flex: '1',
  minH: '0',
  overflowY: 'auto',
  px: '2',
  py: '2',
  display: 'flex',
  flexDirection: 'column',
  gap: '4',
})

const group = css({ display: 'flex', flexDirection: 'column', gap: '0.5' })

const groupHead = css({
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: '2',
  px: '2',
  h: '7',
  fontFamily: 'mono',
  fontSize: '2xs',
  textTransform: 'uppercase',
  letterSpacing: 'wide',
  color: 'fg.subtle',
})

const row = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  w: 'full',
  h: '8',
  px: '2',
  borderRadius: 'sm',
  color: 'fg.muted',
  fontSize: 'sm',
  textAlign: 'left',
  cursor: 'pointer',
  transitionProperty: 'color, background-color',
  transitionDuration: 'fast',
  _hover: { color: 'fg.default', bg: 'surface.hover' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
  '&[data-active]': { color: 'fg.default', bg: 'surface.hover', fontWeight: 'medium' },
})

const rowLabel = css({
  flex: '1',
  minW: '0',
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
})

const count = css({
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
  fontVariantNumeric: 'tabular-nums',
})

const dot = (color: string) => css({ w: '2', h: '2', borderRadius: 'full', flexShrink: '0', bg: color })

const addButton = css({
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '5',
  h: '5',
  borderRadius: 'xs',
  color: 'fg.subtle',
  cursor: 'pointer',
  _hover: { color: 'fg.default', bg: 'surface.hover' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const inlineInput = css({
  w: 'full',
  h: '7',
  px: '2',
  borderRadius: 'sm',
  bg: 'surface.elevated',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border',
  color: 'fg.default',
  fontSize: 'xs',
  _focusVisible: { outline: 'none', borderColor: 'accent' },
})

const footer = css({
  flexShrink: '0',
  px: '3',
  py: '3',
  borderTopWidth: 'hairline',
  borderTopStyle: 'solid',
  borderTopColor: 'border.subtle',
  display: 'flex',
  flexDirection: 'column',
  gap: '2',
})

const status = css({
  display: 'flex',
  alignItems: 'center',
  gap: '2',
  fontFamily: 'mono',
  fontSize: '2xs',
  color: 'fg.subtle',
})

const isActive = (destination: Destination, app: ReturnType<typeof useApp>) => {
  const current = app.state.destination
  if (destination.kind !== current.kind) return false
  return destination.kind === 'tag' && current.kind === 'tag'
    ? destination.tagId === current.tagId
    : true
}

function NavRow(props: {
  icon: JSX.Element
  label: string
  trailing?: JSX.Element
  active: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      class={row}
      data-active={props.active ? '' : undefined}
      onClick={props.onClick}
    >
      <span class={css({ display: 'inline-flex', flexShrink: '0', color: 'fg.subtle' })}>{props.icon}</span>
      <span class={rowLabel}>{props.label}</span>
      {props.trailing}
    </button>
  )
}

export default function NavBar() {
  const app = useApp()
  const [addingTag, setAddingTag] = createSignal(false)
  const [tagDraft, setTagDraft] = createSignal('')

  const openCount = (tagId: string) =>
    app.state.tasks.filter(
      (task) => !task.completed && !app.isBlocked(task) && task.tagIds.includes(tagId),
    ).length

  const openAll = () =>
    app.state.tasks.filter((task) => !task.completed && !app.isBlocked(task)).length

  const commitTag = () => {
    const id = app.addTag(tagDraft())
    setTagDraft('')
    setAddingTag(false)
    if (id) app.setDestination({ kind: 'tag', tagId: id })
  }

  return (
    <nav class={rail} aria-label="Main">
      <div class={brand}>
        <LogoMark size={18} />
        <span>taskstream</span>
      </div>

      <div class={scroll}>
        <div class={group}>
          <NavRow
            icon={<Inbox size={15} />}
            label="All tasks"
            active={app.state.destination.kind === 'all'}
            onClick={() => app.setDestination({ kind: 'all' })}
            trailing={<span class={count}>{openAll()}</span>}
          />
        </div>

        <div class={group}>
          <div class={groupHead}>
            <span>Tags</span>
            <button
              type="button"
              class={addButton}
              aria-label="Add a tag"
              onClick={() => setAddingTag((open) => !open)}
            >
              <Layers size={12} />
            </button>
          </div>
          <Show when={addingTag()}>
            <input
              class={inlineInput}
              value={tagDraft()}
              placeholder="tag name"
              autofocus
              onInput={(event) => setTagDraft(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') commitTag()
                if (event.key === 'Escape') {
                  setAddingTag(false)
                  setTagDraft('')
                }
              }}
              onBlur={() => setAddingTag(false)}
            />
          </Show>
          <For each={app.state.tags}>
            {(tag) => (
              <NavRow
                icon={<span class={dot(tag.color)} />}
                label={`#${tag.name}`}
                active={isActive({ kind: 'tag', tagId: tag.id }, app)}
                onClick={() => app.setDestination({ kind: 'tag', tagId: tag.id })}
                trailing={<span class={count}>{openCount(tag.id)}</span>}
              />
            )}
          </For>
        </div>

        <div class={group}>
          <div class={groupHead}>
            <span>Projects</span>
          </div>
          <For each={app.state.projects}>
            {(project) => (
              <NavRow
                icon={<Folder size={14} />}
                label={project.name}
                active={false}
                onClick={() => app.setDestination({ kind: 'tag', tagId: project.id })}
              />
            )}
          </For>
        </div>

        <div class={group}>
          <NavRow
            icon={<Box size={15} />}
            label="Apps"
            active={app.state.destination.kind === 'apps'}
            onClick={() => app.setDestination({ kind: 'apps' })}
          />
          <NavRow
            icon={<LinkIcon size={15} />}
            label="Integrations"
            active={app.state.destination.kind === 'integrations'}
            onClick={() => app.setDestination({ kind: 'integrations' })}
          />
          <NavRow
            icon={<Zap size={15} />}
            label="Automations"
            active={app.state.destination.kind === 'automations'}
            onClick={() => app.setDestination({ kind: 'automations' })}
          />
          <NavRow
            icon={<Repeat size={15} />}
            label="Sync history"
            active={app.state.destination.kind === 'sync'}
            onClick={() => app.setDestination({ kind: 'sync' })}
          />
          <NavRow
            icon={<Settings size={15} />}
            label="Settings"
            active={app.state.destination.kind === 'settings'}
            onClick={() => app.setDestination({ kind: 'settings' })}
          />
        </div>
      </div>

      <div class={footer}>
        <span class={status}>
          <span class={css({ w: '2', h: '2', borderRadius: 'full', bg: 'success', flexShrink: '0' })} />
          todoist · synced
        </span>
        <span class={status}>local-first · no account</span>
      </div>
    </nav>
  )
}
