export function normalizeChord(event: KeyboardEvent): string | undefined {
  if (event.isComposing || event.keyCode === 229 || event.getModifierState('AltGraph') ||
      ['Alt', 'AltGraph', 'Control', 'Meta', 'Shift', 'Dead', 'Unidentified'].includes(event.key)) return
  const key = event.code === 'Backquote' && (event.ctrlKey || event.metaKey) ? '`'
    : event.key === ' ' ? 'Space' : event.key === '+' ? 'Plus'
      : event.key.length === 1 ? event.key.toUpperCase() : event.key
  return [event.ctrlKey && 'Control', event.altKey && 'Alt', event.shiftKey && 'Shift', event.metaKey && 'Meta', key]
    .filter(Boolean).join('+')
}

export function displayChord(chord: string, mac: boolean) {
  const labels: Record<string, string> = mac
    ? { Control: '⌃', Alt: '⌥', Shift: '⇧', Meta: '⌘', Enter: '↵', Plus: '+' }
    : { Control: 'Ctrl' }
  return chord.split('+').map((key) => labels[key] ?? key).join(mac ? '' : '+')
}
