import { createEffect, createSignal, onCleanup, type JSX } from 'solid-js'

import { sidebarCheckout, type ProjectSnapshot, type TaskSummary } from './bridge'
import { reportError } from './diagnostics'

const menuItems = 'button[role^="menuitem"]'
const cursorGap = 4
const viewportInset = 8

export type SidebarMenuPoint = { x: number; y: number }

export function isSidebarMenuKey(event: KeyboardEvent) {
  return event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')
}

export function isSidebarSelection(event: MouseEvent, mac: boolean) {
  return !event.defaultPrevented && event.button === 0 && !(mac && event.ctrlKey)
}

export function focusSidebarMenu(row: HTMLElement) {
  queueMicrotask(() => row.querySelector<HTMLButtonElement>(`.project-menu ${menuItems}`)?.focus({ preventScroll: true }))
}

export function SidebarMenu(props: {
  row: HTMLElement
  cursor?: SidebarMenuPoint
  label: string
  descriptionId?: string
  onClose: (restoreFocus: boolean) => void
  copyDetails: () => Promise<string>
  children?: JSX.Element
}) {
  const [position, setPosition] = createSignal({ left: 0, top: 0 })
  let menu!: HTMLDivElement

  createEffect(() => {
    const cursor = props.cursor
    const place = () => {
      const height = menu.offsetHeight
      if (cursor) {
        const below = cursor.y + cursorGap
        const top = below + height <= window.innerHeight - viewportInset
          ? below : Math.max(viewportInset, cursor.y - cursorGap - height)
        const left = Math.max(viewportInset, Math.min(cursor.x, window.innerWidth - menu.offsetWidth - viewportInset))
        setPosition({ left, top })
      } else {
        const bounds = props.row.getBoundingClientRect()
        setPosition({ left: bounds.right, top: Math.max(0, Math.min(bounds.top, window.innerHeight - height)) })
      }
    }
    const list = props.row.closest('.projects')
    const dismiss = () => props.onClose(menu.contains(document.activeElement))
    const dismissOutside = (event: PointerEvent) => {
      if (event.target instanceof Node && !props.row.contains(event.target)) props.onClose(false)
    }
    const observer = new ResizeObserver(place)
    place()
    observer.observe(menu)
    const scrollEvent = cursor ? 'wheel' : 'scroll'
    list?.addEventListener(scrollEvent, dismiss)
    window.addEventListener('resize', place)
    document.addEventListener('pointerdown', dismissOutside)
    onCleanup(() => {
      observer.disconnect()
      list?.removeEventListener(scrollEvent, dismiss)
      window.removeEventListener('resize', place)
      document.removeEventListener('pointerdown', dismissOutside)
    })
  })

  async function copyDetails() {
    try {
      // WebKit requires the clipboard write to start in the click gesture, before IPC returns.
      const details = props.copyDetails()
      await navigator.clipboard.write([new ClipboardItem({
        'text/plain': details.then((value) => new Blob([value], { type: 'text/plain' })),
      })])
      if (menu.isConnected) props.onClose(menu.contains(document.activeElement))
    } catch (failure) {
      reportError('copy_sidebar_details', failure)
    }
  }

  return (
    <div ref={menu} class="project-menu" role="menu" aria-label={props.label} aria-describedby={props.descriptionId}
      classList={{ 'project-menu--at-cursor': !!props.cursor }}
      style={{ '--project-menu-left': `${position().left}px`, top: `${position().top}px` }}
      on:contextmenu={(event) => event.preventDefault()}
      onKeyDown={(event) => {
        if (event.key === 'Escape') {
          event.preventDefault()
          event.stopPropagation()
          props.onClose(true)
          return
        }
        if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
        event.preventDefault()
        const items = Array.from(menu.querySelectorAll<HTMLButtonElement>(menuItems))
        const direction = event.key === 'ArrowDown' ? 1 : -1
        const index = items.indexOf(document.activeElement as HTMLButtonElement)
        items[(index + direction + items.length) % items.length]?.focus()
      }}>
      <button type="button" role="menuitem" class="nav-item" tabindex={-1} onClick={() => void copyDetails()}>
        <svg class="project-menu__icon" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true">
          <rect x="5" y="5" width="9" height="9" rx="1.5" />
          <path d="M3.5 11A1.5 1.5 0 0 1 2 9.5v-6A1.5 1.5 0 0 1 3.5 2h6A1.5 1.5 0 0 1 11 3.5" />
        </svg>
        Copy details
      </button>
      {props.children}
    </div>
  )
}

export async function sidebarDetails(project: ProjectSnapshot, task: TaskSummary | undefined, row: 'project' | 'task') {
  const checkoutPath = task?.worktreePath ?? project.path
  let checkout = 'Non-Git directory'
  let branch = '(no branch)'
  if (project.isGitRepository) {
    try {
      const status = await sidebarCheckout(project.id, task?.id ?? null)
      checkout = status.linkedWorktree ? 'Worktree' : 'Main checkout'
      branch = status.branch ?? '(no branch)'
    } catch (failure) {
      reportError('read_sidebar_checkout', failure)
      checkout = 'Unavailable'
      branch = task?.branch ? `${task.branch} (recorded)` : 'Unknown'
    }
  }
  return [
    `Row: ${row === 'project' ? 'Top-level project' : 'Subtask'}`,
    `Project: ${project.name}`,
    `Project path: ${project.path}`,
    `Chat ID: ${task?.id ?? '(none)'}`,
    ...(task ? [`Chat: ${task.title}`] : []),
    `Checkout: ${checkout}`,
    `Checkout path: ${checkoutPath}`,
    `Branch: ${branch}`,
  ].join('\n')
}
