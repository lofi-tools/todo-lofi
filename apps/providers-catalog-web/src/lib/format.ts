import type { BenchmarkUnit } from '../types'

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

export function formatBenchmarkScore(score: number, unit: BenchmarkUnit): string {
  return unit === 'percent' ? `${score.toFixed(1)}%` : score.toFixed(1)
}

export function modelKey(providerId: string, modelId: string): string {
  return `${providerId}/${modelId}`
}

export function providerHref(providerId: string): string {
  return `/providers/${encodeURIComponent(providerId)}`
}

function encodePath(value: string): string {
  return value.split('/').map((segment) => encodeURIComponent(segment)).join('/')
}

export function modelHref(providerId: string, modelId: string): string {
  // Some model ids contain a slash (for example `deepseek-ai/DeepSeek-R1`).
  // Keep those slashes as path separators so the catch-all model route sees them.
  return `/models/${encodeURIComponent(providerId)}/${encodePath(modelId)}`
}

export function canonicalHref(canonicalModelId: string): string {
  return `/models/canonical/${encodePath(canonicalModelId)}`
}

export function harnessHref(harnessId: string): string {
  return `/harnesses/${encodeURIComponent(harnessId)}`
}

export function familyHref(familyId: string): string {
  return `/families/${encodeURIComponent(familyId)}`
}

export function companyHref(companyId: string): string {
  return `/companies/${encodeURIComponent(companyId)}`
}
