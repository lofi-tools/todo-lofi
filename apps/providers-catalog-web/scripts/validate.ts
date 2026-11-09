import path from 'node:path'
import { findLatestSnapshot, validateSnapshotFile } from '../src/snapshot'

const outputDirectory = path.resolve(
  process.env.CATALOG_ARTIFACT_DIR ?? path.join(process.cwd(), 'artifacts'),
)
const requestedPath = process.argv[2] ?? process.env.CATALOG_DUCKDB_PATH
const snapshotPath = requestedPath
  ? path.resolve(requestedPath)
  : await findLatestSnapshot(outputDirectory)

if (!snapshotPath) {
  console.error(`No snapshot found. Run the snapshot command or pass a DuckDB path.`)
  process.exitCode = 1
} else {
  try {
    const report = await validateSnapshotFile(snapshotPath)
    console.log(JSON.stringify({ snapshotPath, ...report }, null, 2))
    if (!report.ok) {
      process.exitCode = 1
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    console.error(`Snapshot validation failed: ${message}`)
    process.exitCode = 1
  }
}
