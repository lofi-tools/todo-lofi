import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { stat } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { parseArgs } from 'node:util'
import { findLatestSnapshot, validateSnapshotFile } from '../src/snapshot'

const packageDirectory = fileURLToPath(new URL('..', import.meta.url))
const defaultArtifactDirectory = path.join(packageDirectory, 'artifacts')

export interface RebuildReport {
  status: 'ready' | 'recovery-required'
  artifactDirectory: string
  snapshotPath: string | null
  stale: boolean | null
  warnings: string[]
  errors: string[]
  recoverySteps: string[]
}

export async function planRebuild(
  options: { snapshot?: string; artifactDirectory?: string } = {},
): Promise<RebuildReport> {
  const artifactDirectory = path.resolve(options.artifactDirectory ?? defaultArtifactDirectory)
  const requestedPath = options.snapshot ? path.resolve(options.snapshot) : null
  const snapshotPath = await resolveSnapshot(requestedPath, artifactDirectory)

  if (!snapshotPath) {
    return recoveryReport(artifactDirectory, requestedPath ?? artifactDirectory, [])
  }

  let validation
  try {
    validation = await validateSnapshotFile(snapshotPath)
  } catch (error) {
    return recoveryReport(artifactDirectory, snapshotPath, [errorMessage(error)])
  }

  if (!validation.ok) {
    return recoveryReport(artifactDirectory, snapshotPath, validation.errors)
  }

  return {
    status: 'ready',
    artifactDirectory,
    snapshotPath,
    stale: validation.stale,
    warnings: validation.warnings,
    errors: [],
    recoverySteps: [],
  }
}

async function resolveSnapshot(
  requestedPath: string | null,
  artifactDirectory: string,
): Promise<string | null> {
  if (requestedPath) {
    if (!existsSync(requestedPath)) {
      return null
    }
    const stats = await stat(requestedPath)
    return stats.isDirectory() ? findLatestSnapshot(requestedPath) : requestedPath
  }
  return findLatestSnapshot(artifactDirectory)
}

function recoveryReport(
  artifactDirectory: string,
  attemptedPath: string,
  errors: string[],
): RebuildReport {
  return {
    status: 'recovery-required',
    artifactDirectory,
    snapshotPath: null,
    stale: null,
    warnings: [],
    errors,
    recoverySteps: [
      `No usable providers-catalog snapshot was found at ${attemptedPath}.`,
      `Place a retained providers-catalog-*.duckdb artifact in ${artifactDirectory}, or run pnpm --dir apps/providers-catalog-web run snapshot for a fresh baseline.`,
      'Retained GitHub Actions artifacts expire after 90 days; after expiry the next successful scrape starts a new baseline because there is no previous artifact to merge.',
      'This command validates and builds locally only. It never deploys.',
    ],
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function runBuild(snapshotPath: string): number {
  const result = spawnSync('pnpm', ['--dir', packageDirectory, 'run', 'build'], {
    env: { ...process.env, CATALOG_DUCKDB_PATH: snapshotPath },
    stdio: 'inherit',
  })
  if (result.error) {
    console.error(result.error.message)
    return 1
  }
  return result.status ?? 1
}

async function main(): Promise<void> {
  const args = process.argv.slice(2)
  if (args[0] === '--') {
    args.shift()
  }
  const { values } = parseArgs({
    args,
    allowPositionals: false,
    options: {
      snapshot: { type: 'string' },
      'artifact-dir': { type: 'string' },
      build: { type: 'boolean', default: false },
      help: { type: 'boolean', default: false },
    },
  })

  if (values.help) {
    console.log(`Usage: pnpm --dir apps/providers-catalog-web run rebuild -- [options]

Options:
  --snapshot <path>     DuckDB file or directory. Defaults to CATALOG_DUCKDB_PATH,
                        then the newest artifacts/*.duckdb.
  --artifact-dir <dir>  Directory used when --snapshot is omitted.
  --build               Build the static site after validation. Never deploys.
  --help                Show this help.`)
    return
  }

  const report = await planRebuild({
    snapshot: values.snapshot,
    artifactDirectory: values['artifact-dir'],
  })
  console.log(JSON.stringify(report, null, 2))

  if (report.status === 'ready' && values.build && report.snapshotPath) {
    process.exitCode = runBuild(report.snapshotPath)
  }
}

const entrypoint = process.argv[1]
if (entrypoint && import.meta.url === pathToFileURL(entrypoint).href) {
  await main()
}
