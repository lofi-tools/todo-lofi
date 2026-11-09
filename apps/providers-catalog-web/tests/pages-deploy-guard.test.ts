import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { pagesDeployDecision } from '../scripts/pages-deploy-guard'

const COMPLETE_ENV: NodeJS.ProcessEnv = {
  CLOUDFLARE_PAGES_ENABLED: 'true',
  CLOUDFLARE_API_TOKEN: 'test-token',
  CLOUDFLARE_ACCOUNT_ID: 'test-account',
  CLOUDFLARE_PAGES_PROJECT: 'test-project',
}

describe('pages deploy guard', () => {
  it('skips when the deployment flag is absent', () => {
    expect(pagesDeployDecision({})).toEqual({
      enabled: false,
      reason: 'CLOUDFLARE_PAGES_ENABLED is not true',
      missing: [],
    })
  })

  it('skips and names missing secrets without failing', () => {
    expect(pagesDeployDecision({ CLOUDFLARE_PAGES_ENABLED: 'true' })).toEqual({
      enabled: false,
      reason:
        'missing Cloudflare configuration: CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_PAGES_PROJECT',
      missing: ['CLOUDFLARE_API_TOKEN', 'CLOUDFLARE_ACCOUNT_ID', 'CLOUDFLARE_PAGES_PROJECT'],
    })
  })

  it('enables only when the flag and all required values are present', () => {
    expect(pagesDeployDecision(COMPLETE_ENV)).toEqual({
      enabled: true,
      reason: 'Cloudflare Pages configuration is present',
      missing: [],
    })
  })

  it('exits zero and writes enabled=false when the CLI has no configuration', () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'pages-guard-'))
    const outputPath = path.join(directory, 'github-output')
    try {
      const output = execFileSync(
        process.execPath,
        ['--experimental-strip-types', 'scripts/pages-deploy-guard.ts'],
        {
          cwd: process.cwd(),
          env: {
            ...process.env,
            CLOUDFLARE_PAGES_ENABLED: '',
            CLOUDFLARE_API_TOKEN: '',
            CLOUDFLARE_ACCOUNT_ID: '',
            CLOUDFLARE_PAGES_PROJECT: '',
            GITHUB_OUTPUT: outputPath,
          },
          encoding: 'utf8',
        },
      )

      expect(output).toContain('deployment disabled')
      expect(readFileSync(outputPath, 'utf8')).toContain('enabled=false')
    } finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it('keeps the workflow guard and deploy steps conditional', () => {
    const workflow = readFileSync(
      path.resolve(process.cwd(), '../../.github/workflows/providers-catalog.yml'),
      'utf8',
    )

    expect(workflow).toContain("if: ${{ vars.CLOUDFLARE_PAGES_ENABLED == 'true' }}")
    expect(workflow).toContain('node --experimental-strip-types')
    expect(workflow).toContain("if: steps.guard.outputs.enabled == 'true'")
    expect(workflow).toContain('cloudflare/wrangler-action@v3')
  })
})
