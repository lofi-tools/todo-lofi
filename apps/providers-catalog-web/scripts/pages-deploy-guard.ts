import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

export interface PagesDeployDecision {
  enabled: boolean
  reason: string
  missing: string[]
}

const REQUIRED_CONFIGURATION = [
  'CLOUDFLARE_API_TOKEN',
  'CLOUDFLARE_ACCOUNT_ID',
  'CLOUDFLARE_PAGES_PROJECT',
] as const

export function pagesDeployDecision(env: NodeJS.ProcessEnv): PagesDeployDecision {
  if (env.CLOUDFLARE_PAGES_ENABLED !== 'true') {
    return {
      enabled: false,
      reason: 'CLOUDFLARE_PAGES_ENABLED is not true',
      missing: [],
    }
  }

  const missing = REQUIRED_CONFIGURATION.filter((name) => !env[name]?.trim())
  if (missing.length > 0) {
    return {
      enabled: false,
      reason: `missing Cloudflare configuration: ${missing.join(', ')}`,
      missing: [...missing],
    }
  }

  return {
    enabled: true,
    reason: 'Cloudflare Pages configuration is present',
    missing: [],
  }
}

function writeGithubOutput(decision: PagesDeployDecision): void {
  const outputPath = process.env.GITHUB_OUTPUT
  if (!outputPath) {
    return
  }

  appendFileSync(
    outputPath,
    `enabled=${decision.enabled}\nreason=${decision.reason}\nmissing=${decision.missing.join(',')}\n`,
    'utf8',
  )
}

const entrypoint = process.argv[1]
if (entrypoint && import.meta.url === pathToFileURL(entrypoint).href) {
  const decision = pagesDeployDecision(process.env)
  writeGithubOutput(decision)
  console.log(
    decision.enabled
      ? 'Cloudflare Pages deployment enabled.'
      : `Cloudflare Pages deployment disabled: ${decision.reason}.`,
  )
}
