import { invoke as nativeInvoke } from '@tauri-apps/api/core'

export interface Diagnostic {
  id: string
  time: string
  operation: string
  message: string
  details: string
}

function errorDetails(error: unknown): { message: string; details: string } {
  const seen = new Set<unknown>()
  const parts: string[] = []
  const messages: string[] = []
  let current: unknown = error
  while (current !== undefined && current !== null && !seen.has(current)) {
    seen.add(current)
    if (typeof current === 'object') {
      const value = current as { message?: unknown; cause?: unknown; causes?: unknown; stack?: unknown; code?: unknown }
      if (value.message) { messages.push(String(value.message)); parts.push(String(value.message)) }
      else {
        try { messages.push(JSON.stringify(current)); parts.push(JSON.stringify(current)) } catch { messages.push(String(current)); parts.push(String(current)) }
      }
      if (value.code) parts.push(`Code: ${String(value.code)}`)
      if (Array.isArray(value.causes)) parts.push(...value.causes.map(String))
      if (value.stack) parts.push(String(value.stack))
      current = value.cause
    } else {
      messages.push(String(current))
      parts.push(String(current))
      break
    }
  }
  const meaningful = messages.find((part) => part && part !== 'Unknown error' && part !== '{}')
  return { message: meaningful ?? 'Unknown error', details: parts.join('\n') || 'No error details were provided.' }
}

let lastReport: { operation: string; details: string; error: unknown; time: number } | undefined

export function reportError(operation: string, error: unknown) {
  const description = errorDetails(error)
  const now = Date.now()
  if (lastReport && now - lastReport.time < 100 && (
    (typeof error === 'object' && error !== null && lastReport.error === error) ||
    (lastReport.operation === operation && lastReport.details === description.details)
  )) return
  lastReport = { operation, details: description.details, error, time: now }
  const record: Diagnostic = {
    id: crypto.randomUUID(), time: new Date().toISOString(), operation,
    message: description.message, details: description.details,
  }
  window.dispatchEvent(new CustomEvent<Diagnostic>('shepherd-diagnostic', { detail: record }))
  void nativeInvoke('record_diagnostic', { record }).catch((failure) => console.error('Could not persist diagnostic', failure))
}

export function diagnosticHistory(): Promise<Diagnostic[]> {
  return nativeInvoke('diagnostic_history')
}
