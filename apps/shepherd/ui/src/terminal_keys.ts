const APP_CURSOR = 1 << 1
const APP_KEYPAD = 1 << 2

export function encodeLegacyKey(event: KeyboardEvent, modes: number): string | null {
  if (event.metaKey || event.getModifierState('AltGraph')) return null
  const key = event.key
  if (event.ctrlKey && event.shiftKey && key.toLowerCase() === 'v') return null
  const modifiers = 1 + (event.shiftKey ? 1 : 0) + (event.altKey ? 2 : 0) + (event.ctrlKey ? 4 : 0)
  const keypad: Record<string, string> = {
    Numpad0: 'p', Numpad1: 'q', Numpad2: 'r', Numpad3: 's', Numpad4: 't',
    Numpad5: 'u', Numpad6: 'v', Numpad7: 'w', Numpad8: 'x', Numpad9: 'y',
    NumpadMultiply: 'j', NumpadAdd: 'k', NumpadSubtract: 'm', NumpadDecimal: 'n',
    NumpadDivide: 'o', NumpadEnter: 'M',
  }
  if ((modes & APP_KEYPAD) && modifiers === 1 && keypad[event.code]) {
    return `\x1bO${keypad[event.code]}`
  }
  const cursor: Record<string, string> = {
    ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D', Home: 'H', End: 'F',
  }
  const functionKey: Record<string, string> = { F1: 'P', F2: 'Q', F3: 'R', F4: 'S' }
  const final = cursor[key] ?? functionKey[key]
  if (final) {
    if (modifiers !== 1) return `\x1b[1;${modifiers}${final}`
    const prefix = functionKey[key] || (modes & APP_CURSOR) ? 'O' : '['
    return `\x1b${prefix}${final}`
  }
  const numbered: Record<string, number> = {
    Insert: 2, Delete: 3, PageUp: 5, PageDown: 6,
    F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24,
  }
  if (numbered[key]) {
    const modifier = modifiers === 1 ? '' : `;${modifiers}`
    return `\x1b[${numbered[key]}${modifier}~`
  }
  if (key === 'Tab' && event.shiftKey) return '\x1b[Z'
  const controls: Record<string, string> = {
    Enter: '\r', Backspace: '\x7f', Tab: '\t', Escape: '\x1b',
  }
  let data = controls[key]
  if (!data && [...key].length === 1) {
    data = key
    if (event.ctrlKey) {
      const code = key.toUpperCase().charCodeAt(0)
      if (key === ' ' || key === '2') data = '\x00'
      else if (key === '?') data = '\x7f'
      else if (code >= 64 && code <= 95) data = String.fromCharCode(code - 64)
    }
  }
  if (!data) return null
  return event.altKey ? `\x1b${data}` : data
}
