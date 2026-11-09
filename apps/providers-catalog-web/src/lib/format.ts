export const UNKNOWN = 'Unknown'

const dateFormatter = new Intl.DateTimeFormat('en-US', {
  dateStyle: 'medium',
  timeZone: 'UTC',
})

const dateTimeFormatter = new Intl.DateTimeFormat('en-US', {
  dateStyle: 'medium',
  timeStyle: 'short',
  timeZone: 'UTC',
})

const numberFormatter = new Intl.NumberFormat('en-US')

export function formatDate(value: string | null): string {
  if (!value) {
    return UNKNOWN
  }
  const date = value.length === 10 ? new Date(`${value}T00:00:00Z`) : new Date(value)
  return Number.isNaN(date.getTime()) ? value : dateFormatter.format(date)
}

export function formatDateTime(value: string | null): string {
  if (!value) {
    return UNKNOWN
  }
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : `${dateTimeFormatter.format(date)} UTC`
}

export function formatNumber(value: number | null): string {
  return value === null ? UNKNOWN : numberFormatter.format(value)
}

export function formatContext(value: number | null): string {
  if (value === null) {
    return UNKNOWN
  }
  if (value >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(value % 1_000_000 === 0 ? 0 : 1)}M`
  }
  if (value >= 1_000) {
    return `${(value / 1_000).toFixed(value % 1_000 === 0 ? 0 : 1)}K`
  }
  return numberFormatter.format(value)
}

export function formatCost(value: number | null): string {
  return value === null ? UNKNOWN : `$${value.toFixed(2)}`
}

export function formatModalities(values: string[] | null): string {
  return values && values.length > 0 ? values.join(', ') : UNKNOWN
}

export function formatList(values: string[]): string {
  return values.length > 0 ? values.join(', ') : UNKNOWN
}

export function formatBoolean(value: boolean | null): string {
  return value === null ? UNKNOWN : value ? 'Yes' : 'No'
}

export function modelKey(providerId: string, modelId: string): string {
  return `${providerId}/${modelId}`
}

export function providerHref(providerId: string): string {
  return `/providers/${encodeURIComponent(providerId)}`
}

export function modelHref(providerId: string, modelId: string): string {
  // Some model ids contain a slash (for example `deepseek-ai/DeepSeek-R1`).
  // Keep those slashes as path separators so the catch-all model route sees them.
  const encodedId = modelId.split('/').map((segment) => encodeURIComponent(segment)).join('/')
  return `/models/${encodeURIComponent(providerId)}/${encodedId}`
}

export function compareHref(keys: string[]): string {
  return `/compare?models=${encodeURIComponent(keys.join(','))}`
}
