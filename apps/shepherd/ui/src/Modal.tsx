import { onCleanup, onMount, type JSX } from 'solid-js'

export function Modal(props: { label: string; title: JSX.Element; onClose: () => void; initialFocus?: () => HTMLElement | undefined; children: JSX.Element }) {
  let dialog: HTMLElement | undefined
  onMount(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : undefined
    props.initialFocus?.()?.focus()
    onCleanup(() => opener?.focus())
  })
  return <div class="picker-backdrop" onClick={props.onClose}>
    <section ref={dialog} class="picker picker-dialog modal" role="dialog" aria-modal="true" aria-label={props.label}
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => {
        if (event.key === 'Escape') { event.preventDefault(); props.onClose(); return }
        if (event.key !== 'Tab' || !dialog) return
        const controls = Array.from(dialog.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href]'))
        if (!controls.length) return
        const index = controls.indexOf(document.activeElement as HTMLElement)
        if ((event.shiftKey && index <= 0) || (!event.shiftKey && index === controls.length - 1)) {
          event.preventDefault()
          ;(event.shiftKey ? controls.at(-1) : controls[0])?.focus()
        }
      }}>
      <ModalHeader>{props.title}</ModalHeader>
      <div class="modal__body">{props.children}</div>
    </section>
  </div>
}

export function ModalHeader(props: { children: JSX.Element }) {
  return <header class="picker__header modal__header"><h3>{props.children}</h3></header>
}

export function ModalActions(props: { children: JSX.Element }) {
  return <div class="modal__actions">{props.children}</div>
}
