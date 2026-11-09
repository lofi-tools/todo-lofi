import path from 'node:path'
import { fetchModelsDevCatalog, MODELS_DEV_SOURCE } from '../src/models-dev'
import { createSnapshot, findLatestSnapshot, recordSnapshotFailure } from '../src/snapshot'

const outputDirectory = path.resolve(
  process.env.CATALOG_ARTIFACT_DIR ?? path.join(process.cwd(), 'artifacts'),
)

try {
  const fetched = await fetchModelsDevCatalog()
  const previousSnapshotPath = await findLatestSnapshot(outputDirectory)
  const report = await createSnapshot({
    catalog: fetched.catalog,
    source: { ...MODELS_DEV_SOURCE, sourceUrl: fetched.sourceUrl },
    fetchedAt: fetched.fetchedAt,
    contentSha256: fetched.contentSha256,
    httpStatus: fetched.httpStatus,
    etag: fetched.etag,
    outputDirectory,
    previousSnapshotPath: previousSnapshotPath ?? undefined,
  })

  if (report.merge && report.merge.reviewItems.length > 0) {
    console.warn(
      `Preserved ${report.merge.reviewItems.length} missing facts for review: ${report.merge.reviewItems.join(', ')}`,
    )
  }

  console.log(
    JSON.stringify(
      {
        status: 'ok',
        source: MODELS_DEV_SOURCE.sourceUrl,
        ...report,
      },
      null,
      2,
    ),
  )
} catch (error) {
  const message = error instanceof Error ? error.message : String(error)
  try {
    const failurePath = await recordSnapshotFailure({
      outputDirectory,
      sourceUrl: MODELS_DEV_SOURCE.sourceUrl,
      error,
    })
    console.error(`Snapshot failed; wrote ${failurePath}`)
  } catch (recordError) {
    const recordMessage =
      recordError instanceof Error ? recordError.message : String(recordError)
    console.error(`Snapshot failed and the failure record could not be written: ${recordMessage}`)
  }
  console.error(message)
  process.exitCode = 1
}
