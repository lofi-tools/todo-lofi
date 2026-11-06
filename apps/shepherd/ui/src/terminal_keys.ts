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

const DISAMBIGUATE = 1 << 18
export const KITTY_REPORT_EVENTS = 1 << 19
const ALTERNATE_KEYS = 1 << 20
const ALL_KEYS = 1 << 21
const ASSOCIATED_TEXT = 1 << 22
const KITTY_MODES = DISAMBIGUATE | KITTY_REPORT_EVENTS | ALTERNATE_KEYS | ALL_KEYS | ASSOCIATED_TEXT

const FUNCTIONAL_KEYS: Record<string, string> = {
  Escape: '27u', Enter: '13u', Tab: '9u', Backspace: '127u',
  Insert: '2~', Delete: '3~', PageUp: '5~', PageDown: '6~',
  ArrowUp: '1A', ArrowDown: '1B', ArrowRight: '1C', ArrowLeft: '1D', Home: '1H', End: '1F',
  F1: '1P', F2: '1Q', F3: '13~', F4: '1S', F5: '15~', F6: '17~',
  F7: '18~', F8: '19~', F9: '20~', F10: '21~', F11: '23~', F12: '24~',
  CapsLock: '57358u', ScrollLock: '57359u', NumLock: '57360u', PrintScreen: '57361u',
  Pause: '57362u', ContextMenu: '57363u',
  MediaPlay: '57428u', MediaPause: '57429u', MediaPlayPause: '57430u', MediaStop: '57432u',
  MediaTrackNext: '57435u', MediaTrackPrevious: '57436u',
  AudioVolumeDown: '57438u', AudioVolumeUp: '57439u', AudioVolumeMute: '57440u',
}

const KEYPAD_KEYS: Record<string, number> = {
  NumpadDecimal: 57409, NumpadDivide: 57410, NumpadMultiply: 57411,
  NumpadSubtract: 57412, NumpadAdd: 57413, NumpadEnter: 57414,
  NumpadEqual: 57415, NumpadComma: 57416,
}

const KEYPAD_NAVIGATION: Record<string, number> = {
  ArrowLeft: 57417, ArrowRight: 57418, ArrowUp: 57419, ArrowDown: 57420,
  PageUp: 57421, PageDown: 57422, Home: 57423, End: 57424, Insert: 57425, Delete: 57426,
  Clear: 57427,
}

const MODIFIER_KEYS: Record<string, number> = {
  ShiftLeft: 57441, ControlLeft: 57442, AltLeft: 57443, MetaLeft: 57444,
  ShiftRight: 57447, ControlRight: 57448, AltRight: 57449, MetaRight: 57450,
}

const US_KEYS: Record<string, string> = {
  Backquote: '`', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']',
  Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/',
}

const US_SHIFT = '~_+{}|:"<>?'
const US_DIGIT_SHIFT = ')!@#$%^&*('

function unshifted(key: string, code: string): [string, string | undefined] {
  const letter = /^Key([A-Z])$/.exec(code)?.[1].toLowerCase()
  const digit = /^Digit([0-9])$/.exec(code)?.[1]
  const base = letter ?? digit ?? US_KEYS[code]
  if (digit && key === US_DIGIT_SHIFT[Number(digit)]) return [digit, base]
  const punctuation = Object.keys(US_KEYS).indexOf(code)
  if (punctuation >= 0 && key === US_SHIFT[punctuation]) return [US_KEYS[code], base]
  return [key.toLowerCase(), base]
}

interface KittyKeyPress {
  data: string
  release?: (event?: KeyboardEvent) => string
}

export function encodeKittyKey(event: KeyboardEvent, modes: number): KittyKeyPress | null {
  if (!(modes & KITTY_MODES) || event.isComposing || event.getModifierState('AltGraph')) return null
  const key = event.key
  const all = Boolean(modes & ALL_KEYS)
  const reportEvents = Boolean(modes & KITTY_REPORT_EVENTS)

  const modified = event.ctrlKey || event.altKey || event.metaKey
  const printable = [...key].length === 1
  const [primary, base] = printable ? unshifted(key, event.code) : ['', undefined]
  let code = FUNCTIONAL_KEYS[key]
  const fn = /^F(1[3-9]|2[0-9]|3[0-5])$/.exec(key)
  if (fn) code = `${57376 + Number(fn[1]) - 13}u`
  if (event.location === KeyboardEvent.DOM_KEY_LOCATION_NUMPAD && (all || ((modes & DISAMBIGUATE) && (!printable || modified)))) {
    const digit = /^Numpad([0-9])$/.exec(event.code)?.[1]
    const number = KEYPAD_NAVIGATION[key] ?? KEYPAD_KEYS[event.code] ?? (digit === undefined ? undefined : 57399 + Number(digit))
    if (number !== undefined) code = `${number}u`
  }
  if (all && MODIFIER_KEYS[event.code]) code = `${MODIFIER_KEYS[event.code]}u`
  if (!all && ['Enter', 'Tab', 'Backspace'].includes(key) && code === FUNCTIONAL_KEYS[key]) return null
  if (!all && key === 'Escape' && !(modes & (DISAMBIGUATE | KITTY_REPORT_EVENTS))) return null
  if (!all && code && !(modes & (DISAMBIGUATE | KITTY_REPORT_EVENTS))) return null
  if (!code) {
    if (!printable || (!all && (!(modes & DISAMBIGUATE) || !modified))) return null
    code = `${primary.codePointAt(0)}u`
  }

  const final = code.slice(-1)
  let keyCode = code.slice(0, -1)
  if (final === 'u' && printable && keyCode === String(primary.codePointAt(0)) && modes & ALTERNATE_KEYS) {
    const shifted = event.shiftKey && key !== primary ? key.codePointAt(0) : undefined
    const baseCode = base && base !== primary ? base.codePointAt(0) : undefined
    if (baseCode !== undefined) keyCode += `:${shifted ?? ''}:${baseCode}`
    else if (shifted !== undefined) keyCode += `:${shifted}`
  }
  const locks = lockModifiers(event, all)
  const modifiers = 1 + keyModifiers(event) + locks
  const kind = reportEvents && event.repeat ? 2 : 0
  const associated = all && (modes & ASSOCIATED_TEXT) && !modified && printable
    ? textCodePoints(key)
    : ''
  const modifierField = modifiers !== 1 || kind ? `${modifiers}${kind ? `:${kind}` : ''}` : ''
  const parameters = modifierField || associated
    ? `${keyCode};${modifierField}${associated ? `;${associated}` : ''}`
    : (keyCode === '1' && final !== 'u' ? '' : keyCode)
  return {
    data: `\x1b[${parameters}${final}`,
    // The released key keeps the identity reported on keydown even if its modifiers changed.
    release: reportEvents ? (event) => {
      const modifiers = 1 + (event ? keyModifiers(event) + lockModifiers(event, all) : locks)
      return `\x1b[${keyCode};${modifiers}:3${final}`
    } : undefined,
  }
}

function keyModifiers(event: KeyboardEvent): number {
  return (event.shiftKey ? 1 : 0) + (event.altKey ? 2 : 0)
    + (event.ctrlKey ? 4 : 0) + (event.metaKey ? 8 : 0)
}

function lockModifiers(event: KeyboardEvent, all: boolean): number {
  return (all && event.getModifierState('CapsLock') ? 64 : 0)
    + (all && event.getModifierState('NumLock') ? 128 : 0)
}

export function encodeTerminalText(text: string, modes: number): string {
  if (!(modes & ALL_KEYS)) return text
  if (!(modes & ASSOCIATED_TEXT)) return ''
  const points = textCodePoints(text)
  return points ? `\x1b[0;;${points}u` : ''
}

function textCodePoints(text: string): string {
  return Array.from(text, (character) => character.codePointAt(0)!)
    .filter((point) => point >= 32 && (point < 127 || point >= 160)).join(':')
}
