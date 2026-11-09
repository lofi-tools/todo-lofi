(() => {
  const { protocol, hostname } = location
  const loopback = hostname === 'localhost' || hostname === '[::1]' || /^127\.(?:\d{1,3}\.){2}\d{1,3}$/.test(hostname)
  if (window === window.top || window.parent !== window.top || !loopback || !['http:', 'https:'].includes(protocol)) return

  let cancelSelection

  const styleProperties = {
    backgroundColor: 'background-color', border: 'border', borderRadius: 'border-radius',
    color: 'color', display: 'display', fontSize: 'font-size', fontWeight: 'font-weight',
    margin: 'margin', padding: 'padding',
  }

  function selectorFor(element) {
    const parts = []
    for (let node = element; node && parts.length < 40; node = node.parentElement) {
      if (node.id && node.id.length <= 160) {
        const id = `#${CSS.escape(node.id)}`
        if (document.querySelectorAll(id).length === 1) {
          parts.unshift(id)
          break
        }
      }
      let index = 1
      for (let sibling = node.previousElementSibling; sibling; sibling = sibling.previousElementSibling) {
        if (sibling.localName === node.localName && sibling.namespaceURI === node.namespaceURI) index += 1
      }
      parts.unshift(`${CSS.escape(node.localName)}:nth-of-type(${index})`)
    }
    const selector = parts.join(' > ')
    if (selector.length > 2048) return null
    const matches = document.querySelectorAll(selector)
    return matches.length === 1 && matches[0] === element ? selector : null
  }

  function selectElement(port) {
    cancelSelection?.()
    const overlay = document.createElement('div')
    const outline = document.createElement('div')
    Object.assign(overlay.style, {
      all: 'initial', position: 'fixed', inset: '0', zIndex: '2147483647',
      cursor: 'crosshair', touchAction: 'none', overscrollBehavior: 'none',
    })
    Object.assign(outline.style, {
      all: 'initial', position: 'fixed', pointerEvents: 'none',
      outline: '2px solid #6387ff', background: 'rgba(99, 135, 255, 0.14)',
    })
    overlay.tabIndex = -1
    overlay.setAttribute('aria-label', 'Choose an element. Press Escape to cancel.')
    overlay.append(outline)
    document.documentElement.append(overlay)
    overlay.focus({ preventScroll: true })

    function finish(result) {
      overlay.remove()
      cancelSelection = undefined
      if (result) port.postMessage(result)
      port.close()
    }

    const target = (event) => document.elementsFromPoint(event.clientX, event.clientY)
      .find((element) => !overlay.contains(element))
    cancelSelection = () => finish({ type: 'cancelled' })
    port.onmessage = ({ data }) => { if (data === 'cancel') finish() }
    overlay.addEventListener('wheel', (event) => event.preventDefault(), { passive: false })
    overlay.addEventListener('keydown', (event) => {
      event.stopPropagation()
      event.preventDefault()
      if (event.key === 'Escape') cancelSelection?.()
    })
    overlay.addEventListener('pointermove', (event) => {
      const bounds = target(event)?.getBoundingClientRect()
      if (!bounds) return
      Object.assign(outline.style, {
        left: `${bounds.x}px`, top: `${bounds.y}px`,
        width: `${bounds.width}px`, height: `${bounds.height}px`,
      })
    })
    overlay.addEventListener('click', (event) => {
      event.preventDefault()
      event.stopPropagation()
      const element = target(event)
      const selector = element && !element.matches('iframe, frame, object, embed') && selectorFor(element)
      if (!selector) {
        finish({ type: 'unavailable' })
        return
      }
      const { x, y, width, height } = element.getBoundingClientRect()
      const excerpt = (element.innerText ?? element.textContent ?? '').replace(/\s+/g, ' ').trim().slice(0, 160)
      const computed = getComputedStyle(element)
      const computedStyles = Object.fromEntries(Object.entries(styleProperties)
        .map(([name, property]) => [name, computed.getPropertyValue(property)]))
      finish({ type: 'selected', url: location.href, selector, excerpt, bounds: { x, y, width, height }, computedStyles })
    })
    port.postMessage({ type: 'ready' })
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window.parent || event.data !== 'shepherd:annotate-preview' || event.ports.length !== 1) return
    const [port] = event.ports
    if (document.contentType === 'text/html' && document.documentElement) selectElement(port)
    else {
      port.postMessage({ type: 'unavailable' })
      port.close()
    }
  })
})()
