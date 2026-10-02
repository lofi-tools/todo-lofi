import { For, Show } from 'solid-js'
import { css, cx } from 'styled-system/css'
import { Bell } from 'web-design-system/icons'
import { Popover } from 'web-design-system/headless'
import { useApp } from '../store'

const trigger = css({
  position: 'relative',
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  w: '7',
  h: '7',
  borderRadius: 'sm',
  color: 'fg.muted',
  cursor: 'pointer',
  _hover: { color: 'fg.default', bg: 'surface.hover' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const badge = css({
  position: 'absolute',
  top: '-1',
  right: '-1',
  display: 'inline-flex',
  alignItems: 'center',
  justifyContent: 'center',
  minW: '4',
  h: '4',
  px: '1',
  borderRadius: 'full',
  bg: 'accent',
  color: 'fg.onAccent',
  fontSize: '2xs',
  fontWeight: 'semibold',
})

const content = css({
  w: '80',
  maxW: '92vw',
  bg: 'surface.elevated',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border',
  borderRadius: 'lg',
  boxShadow: 'lg',
  overflow: 'hidden',
})

const head = css({
  display: 'flex',
  alignItems: 'center',
  justifyContent: 'space-between',
  gap: '2',
  px: '3',
  h: '10',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
  fontSize: 'xs',
  fontWeight: 'semibold',
  color: 'fg.default',
})

const link = css({
  color: 'fg.subtle',
  fontSize: '2xs',
  cursor: 'pointer',
  _hover: { color: 'fg.default' },
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
})

const list = css({ display: 'flex', flexDirection: 'column', maxH: '72', overflowY: 'auto' })

const item = css({
  display: 'flex',
  gap: '2.5',
  px: '3',
  py: '2.5',
  borderBottomWidth: 'hairline',
  borderBottomStyle: 'solid',
  borderBottomColor: 'border.subtle',
  '&:last-child': { borderBottomWidth: '0' },
})

const toneDot = (tone: string) =>
  css({
    w: '2',
    h: '2',
    mt: '1.5',
    borderRadius: 'full',
    flexShrink: '0',
    bg:
      tone === 'success'
        ? 'success'
        : tone === 'warning'
          ? 'warning'
          : tone === 'danger'
            ? 'danger'
            : 'accent',
  })

const noticeTitle = css({ fontSize: 'xs', color: 'fg.default' })
const noticeBody = css({ fontSize: '2xs', color: 'fg.muted', lineHeight: 'relaxed' })
const noticeTime = css({ fontFamily: 'mono', fontSize: '2xs', color: 'fg.subtle' })

const empty = css({ px: '3', py: '6', textAlign: 'center', fontSize: 'xs', color: 'fg.subtle' })

export default function NotificationsPanel() {
  const app = useApp()

  return (
    <Popover.Root
      open={app.state.notificationsOpen}
      onOpenChange={(details) => (details.open ? app.openNotifications() : app.closeNotifications())}
      positioning={{ placement: 'top-end', gutter: 8 }}
    >
      <Popover.Trigger class={trigger} aria-label="Notifications">
        <Bell size={15} />
        <Show when={app.unreadCount() > 0}>
          <span class={badge}>{app.unreadCount()}</span>
        </Show>
      </Popover.Trigger>
      <Popover.Positioner class={css({ zIndex: 'popover' })}>
        <Popover.Content class={content}>
          <div class={head}>
            <span>Notifications</span>
            <span class={css({ display: 'inline-flex', gap: '3' })}>
              <button type="button" class={link} onClick={app.markAllRead}>
                Mark all read
              </button>
              <button type="button" class={link} onClick={app.clearNotifications}>
                Clear
              </button>
            </span>
          </div>

          <Show
            when={app.state.notifications.length > 0}
            fallback={<p class={empty}>Nothing to catch up on.</p>}
          >
            <div class={list}>
              <For each={app.state.notifications}>
                {(notice) => (
                  <div class={item}>
                    <span class={cx(toneDot(notice.tone), notice.unread ? undefined : css({ opacity: '0.4' }))} />
                    <div class={css({ minW: '0', flex: '1' })}>
                      <div class={noticeTitle}>{notice.title}</div>
                      <div class={noticeBody}>{notice.body}</div>
                      <div class={noticeTime}>{notice.time}</div>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </Show>
        </Popover.Content>
      </Popover.Positioner>
    </Popover.Root>
  )
}
