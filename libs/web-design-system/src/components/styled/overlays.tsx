import type { JSX } from 'solid-js'
import { For, Show } from 'solid-js'
import { Dialog as ArkDialog, Menu as ArkMenu, Popover as ArkPopover, Tooltip as ArkTooltip } from '../headless'
import { X } from '../../icons'
import { css } from '../../../styled-system/css'

const surface = {
  bg: 'surface.elevated',
  borderWidth: 'hairline',
  borderStyle: 'solid',
  borderColor: 'border',
  borderRadius: 'lg',
  boxShadow: 'lg',
  color: 'fg.default',
}

export interface DialogProps {
  trigger: string
  title: string
  description?: string
  /** Plain-text body, so the Astro wrapper never needs a slot (client:only
   *  islands cannot receive projected slot content). */
  body?: string
  children?: JSX.Element
  class?: string
}

export function Dialog(props: DialogProps): JSX.Element {
  return (
    <ArkDialog.Root>
      <ArkDialog.Trigger
        class={css({
          display: 'inline-flex',
          alignItems: 'center',
          h: '9',
          px: '4',
          borderRadius: 'sm',
          bg: 'surface.elevated',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          borderColor: 'border',
          color: 'fg.default',
          fontSize: 'md',
          fontWeight: 'medium',
          cursor: 'pointer',
          _hover: { bg: 'surface.hover' },
          _focusVisible: { outline: 'none', boxShadow: 'focus' },
        })}
      >
        {props.trigger}
      </ArkDialog.Trigger>
      <ArkDialog.Backdrop
          class={css({
            position: 'fixed',
            inset: '0',
            bg: 'overlay',
            zIndex: 'overlay',
            animation: 'fadeIn 0.15s ease-out',
          })}
        />
        <ArkDialog.Positioner
          class={css({
            position: 'fixed',
            inset: '0',
            display: 'grid',
            placeItems: 'center',
            zIndex: 'overlay',
            p: '4',
          })}
        >
          <ArkDialog.Content
            class={css({
              ...surface,
              w: 'full',
              maxW: '440px',
              p: '6',
              position: 'relative',
              animation: 'slideUp 0.18s ease-out',
            })}
          >
            <ArkDialog.Title class={css({ fontSize: 'lg', fontWeight: 'semibold' })}>
              {props.title}
            </ArkDialog.Title>
            <Show when={props.description}>
              <ArkDialog.Description
                class={css({ mt: '2', fontSize: 'sm', color: 'fg.muted', lineHeight: 'relaxed' })}
              >
                {props.description}
              </ArkDialog.Description>
            </Show>
            <Show when={props.children || props.body}>
              <div class={css({ mt: '4', fontSize: 'sm', color: 'fg.muted', lineHeight: 'relaxed' })}>
                {props.children ?? props.body}
              </div>
            </Show>
            <ArkDialog.CloseTrigger
              aria-label="Close dialog"
              class={css({
                position: 'absolute',
                top: '4',
                right: '4',
                display: 'inline-flex',
                alignItems: 'center',
                justifyContent: 'center',
                w: '7',
                h: '7',
                borderRadius: 'sm',
                color: 'fg.muted',
                cursor: 'pointer',
                _hover: { bg: 'surface.hover', color: 'fg.default' },
                _focusVisible: { outline: 'none', boxShadow: 'focus' },
              })}
            >
              <X size={15} />
            </ArkDialog.CloseTrigger>
          </ArkDialog.Content>
        </ArkDialog.Positioner>
    </ArkDialog.Root>
  )
}

export interface PopoverProps {
  trigger: string
  body?: string
  children?: JSX.Element
  class?: string
}

export function Popover(props: PopoverProps): JSX.Element {
  return (
    <ArkPopover.Root>
      <ArkPopover.Trigger
        class={css({
          display: 'inline-flex',
          alignItems: 'center',
          h: '9',
          px: '4',
          borderRadius: 'sm',
          bg: 'surface.elevated',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          borderColor: 'border',
          color: 'fg.default',
          fontSize: 'md',
          fontWeight: 'medium',
          cursor: 'pointer',
          _hover: { bg: 'surface.hover' },
          _focusVisible: { outline: 'none', boxShadow: 'focus' },
        })}
      >
        {props.trigger}
      </ArkPopover.Trigger>
      <ArkPopover.Positioner class={css({ zIndex: 'popover' })}>
          <ArkPopover.Content
            class={css({ ...surface, p: '4', minW: '220px', fontSize: 'sm' })}
          >
            {props.children ?? props.body}
          </ArkPopover.Content>
      </ArkPopover.Positioner>
    </ArkPopover.Root>
  )
}

export interface TooltipProps {
  label: string
  /** Trigger text; a custom element can be passed as `children` instead. */
  trigger?: string
  children?: JSX.Element
  class?: string
}

export function Tooltip(props: TooltipProps): JSX.Element {
  const trigger = () =>
    props.children ?? (
      <span class={css({ borderBottomWidth: 'hairline', borderStyle: 'dashed', borderColor: 'border.strong', cursor: 'help' })}>
        {props.trigger}
      </span>
    )
  return (
    <ArkTooltip.Root>
      <ArkTooltip.Trigger>{trigger()}</ArkTooltip.Trigger>
      <ArkTooltip.Positioner class={css({ zIndex: 'popover' })}>
          <ArkTooltip.Content
            class={css({
              bg: 'surface.elevated',
              color: 'fg.default',
              borderWidth: 'hairline',
              borderStyle: 'solid',
              borderColor: 'border',
              borderRadius: 'sm',
              px: '2',
              py: '1',
              fontSize: 'xs',
              boxShadow: 'md',
            })}
          >
            {props.label}
          </ArkTooltip.Content>
      </ArkTooltip.Positioner>
    </ArkTooltip.Root>
  )
}

export interface MenuItem {
  value: string
  label: string
  disabled?: boolean
}

export interface MenuProps {
  trigger: string
  items: MenuItem[]
  class?: string
}

export function Menu(props: MenuProps): JSX.Element {
  return (
    <ArkMenu.Root>
      <ArkMenu.Trigger
        class={css({
          display: 'inline-flex',
          alignItems: 'center',
          h: '9',
          px: '4',
          borderRadius: 'sm',
          bg: 'surface.elevated',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          borderColor: 'border',
          color: 'fg.default',
          fontSize: 'md',
          fontWeight: 'medium',
          cursor: 'pointer',
          _hover: { bg: 'surface.hover' },
          _focusVisible: { outline: 'none', boxShadow: 'focus' },
        })}
      >
        {props.trigger}
      </ArkMenu.Trigger>
      <ArkMenu.Positioner class={css({ zIndex: 'popover' })}>
          <ArkMenu.Content
            class={css({ ...surface, p: '1', minW: '200px', display: 'flex', flexDirection: 'column' })}
          >
            <For each={props.items}>
              {(item) => (
                <ArkMenu.Item
                  value={item.value}
                  disabled={item.disabled}
                  class={css({
                    display: 'flex',
                    alignItems: 'center',
                    px: '2',
                    h: '8',
                    borderRadius: 'sm',
                    fontSize: 'sm',
                    color: 'fg.default',
                    cursor: 'pointer',
                    _hover: { bg: 'surface.hover' },
                    _disabled: { opacity: '0.5', cursor: 'not-allowed' },
                    _focusVisible: { outline: 'none', bg: 'surface.hover' },
                  })}
                >
                  {item.label}
                </ArkMenu.Item>
              )}
            </For>
          </ArkMenu.Content>
      </ArkMenu.Positioner>
    </ArkMenu.Root>
  )
}
