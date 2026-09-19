import type { JSX } from 'solid-js'
import { For, splitProps } from 'solid-js'
import { Accordion as ArkAccordion, Checkbox as ArkCheckbox, Switch as ArkSwitch, Tabs as ArkTabs } from '../headless'
import { Check, ChevronDown } from '../../icons'
import { css, cx } from '../../../styled-system/css'

const focusRing = {
  _focusVisible: { outline: 'none', boxShadow: 'focus' },
}

export interface TabItem {
  value: string
  label: string
  content?: string
}

export interface TabsProps {
  tabs: TabItem[]
  defaultValue?: string
  class?: string
}

export function Tabs(props: TabsProps): JSX.Element {
  return (
    <ArkTabs.Root
      defaultValue={props.defaultValue ?? props.tabs[0]?.value}
      class={cx(css({ display: 'flex', flexDirection: 'column', gap: '4' }), props.class)}
    >
      <ArkTabs.List
        class={css({
          display: 'inline-flex',
          gap: '1',
          p: '1',
          borderRadius: 'md',
          bg: 'surface.subtle',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          borderColor: 'border',
          alignSelf: 'flex-start',
          position: 'relative',
        })}
      >
        <For each={props.tabs}>
          {(tab) => (
            <ArkTabs.Trigger
              value={tab.value}
              class={css({
                px: '3',
                h: '7',
                display: 'inline-flex',
                alignItems: 'center',
                borderRadius: 'sm',
                fontSize: 'sm',
                fontWeight: 'medium',
                color: 'fg.muted',
                cursor: 'pointer',
                transitionProperty: 'color, background-color',
                transitionDuration: 'fast',
                _hover: { color: 'fg.default' },
                '&[data-selected]': { color: 'fg.default', bg: 'surface.elevated' },
                ...focusRing,
              })}
            >
              {tab.label}
            </ArkTabs.Trigger>
          )}
        </For>
      </ArkTabs.List>
      <For each={props.tabs}>
        {(tab) => (
          <ArkTabs.Content
            value={tab.value}
            class={css({
              color: 'fg.muted',
              fontSize: 'md',
              lineHeight: 'relaxed',
              _focusVisible: { outline: 'none' },
            })}
          >
            {tab.content}
          </ArkTabs.Content>
        )}
      </For>
    </ArkTabs.Root>
  )
}

export interface AccordionItem {
  value: string
  trigger: string
  content: string
}

export interface AccordionProps {
  items: AccordionItem[]
  class?: string
}

export function Accordion(props: AccordionProps): JSX.Element {
  return (
    <ArkAccordion.Root
      collapsible
      class={cx(
        css({
          display: 'flex',
          flexDirection: 'column',
          borderTopWidth: 'hairline',
          borderColor: 'border',
        }),
        props.class,
      )}
    >
      <For each={props.items}>
        {(item) => (
          <ArkAccordion.Item
            value={item.value}
            class={css({ borderBottomWidth: 'hairline', borderColor: 'border' })}
          >
            <ArkAccordion.ItemTrigger
              class={css({
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
                gap: '4',
                w: 'full',
                py: '4',
                textAlign: 'left',
                fontSize: 'md',
                fontWeight: 'medium',
                color: 'fg.default',
                cursor: 'pointer',
                _hover: { color: 'accent.text' },
                ...focusRing,
              })}
            >
              {item.trigger}
              <ArkAccordion.ItemIndicator
                class={css({
                  color: 'fg.muted',
                  transitionProperty: 'transform',
                  transitionDuration: 'normal',
                  '&[data-state="open"]': { transform: 'rotate(180deg)' },
                })}
              >
                <ChevronDown size={15} />
              </ArkAccordion.ItemIndicator>
            </ArkAccordion.ItemTrigger>
            <ArkAccordion.ItemContent
              class={css({
                pb: '4',
                fontSize: 'sm',
                lineHeight: 'relaxed',
                color: 'fg.muted',
              })}
            >
              {item.content}
            </ArkAccordion.ItemContent>
          </ArkAccordion.Item>
        )}
      </For>
    </ArkAccordion.Root>
  )
}

export interface SwitchProps {
  label: string
  defaultChecked?: boolean
  disabled?: boolean
  class?: string
}

export function Switch(props: SwitchProps): JSX.Element {
  const [local] = splitProps(props, ['label', 'defaultChecked', 'disabled', 'class'])
  return (
    <ArkSwitch.Root
      defaultChecked={local.defaultChecked}
      disabled={local.disabled}
      class={cx(
        css({ display: 'inline-flex', alignItems: 'center', gap: '3', cursor: 'pointer' }),
        local.class,
      )}
    >
      <ArkSwitch.Control
        class={css({
          display: 'inline-flex',
          alignItems: 'center',
          w: '9',
          h: '5',
          p: '0.5',
          borderRadius: 'full',
          bg: 'surface.hover',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          borderColor: 'border',
          transitionProperty: 'background-color',
          transitionDuration: 'fast',
          '&[data-state="checked"]': { bg: 'accent', borderColor: 'accent' },
          ...focusRing,
        })}
      >
        <ArkSwitch.Thumb
          class={css({
            w: '4',
            h: '4',
            borderRadius: 'full',
            bg: 'fg.default',
            transitionProperty: 'transform',
            transitionDuration: 'fast',
            '&[data-state="checked"]': { transform: 'translateX(16px)' },
          })}
        />
      </ArkSwitch.Control>
      <ArkSwitch.Label class={css({ fontSize: 'sm', color: 'fg.default' })}>
        {local.label}
      </ArkSwitch.Label>
      <ArkSwitch.HiddenInput />
    </ArkSwitch.Root>
  )
}

export interface CheckboxProps {
  label: string
  defaultChecked?: boolean
  disabled?: boolean
  class?: string
}

export function Checkbox(props: CheckboxProps): JSX.Element {
  return (
    <ArkCheckbox.Root
      defaultChecked={props.defaultChecked}
      disabled={props.disabled}
      class={cx(
        css({ display: 'inline-flex', alignItems: 'center', gap: '3', cursor: 'pointer' }),
        props.class,
      )}
    >
      <ArkCheckbox.Control
        class={css({
          display: 'inline-grid',
          placeItems: 'center',
          w: '4.5',
          h: '4.5',
          borderRadius: 'xs',
          borderWidth: 'hairline',
          borderStyle: 'solid',
          borderColor: 'border.strong',
          bg: 'surface.subtle',
          color: 'transparent',
          transitionProperty: 'background-color, border-color, color',
          transitionDuration: 'fast',
          '&[data-state="checked"]': {
            bg: 'accent',
            borderColor: 'accent',
            color: 'fg.onAccent',
          },
          ...focusRing,
        })}
      >
        <ArkCheckbox.Indicator>
          <Check size={12} />
        </ArkCheckbox.Indicator>
      </ArkCheckbox.Control>
      <ArkCheckbox.Label class={css({ fontSize: 'sm', color: 'fg.default' })}>
        {props.label}
      </ArkCheckbox.Label>
      <ArkCheckbox.HiddenInput />
    </ArkCheckbox.Root>
  )
}
