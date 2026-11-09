import path from 'node:path'
import { fetchModelsDevCatalog, MODELS_DEV_SOURCE } from '../src/models-dev'
import { SCHEMA_VERSION } from '../src/schema'
import {
  createSnapshot,
  findLatestSnapshot,
  readSnapshotSchemaVersion,
  recordSnapshotFailure,
} from '../src/snapshot'

const outputDirectory = path.resolve(
  process.env.CATALOG_ARTIFACT_DIR ?? path.join(process.cwd(), 'artifacts'),
)

try {
  const fetched = await fetchModelsDevCatalog()
  const latestSnapshotPath = await findLatestSnapshot(outputDirectory)
  let previousSnapshotPath: string | undefined
  if (latestSnapshotPath) {
    const previousSchema = await readSnapshotSchemaVersion(latestSnapshotPath)
    if (previousSchema === SCHEMA_VERSION) {
      previousSnapshotPath = latestSnapshotPath
    } else {
      console.warn(
        `Skipping merge: ${latestSnapshotPath} has schema ${previousSchema}, and schema ${SCHEMA_VERSION} starts a new baseline.`,
      )
    }
  }

  const report = await createSnapshot({
    catalog: fetched.catalog,
    source: { ...MODELS_DEV_SOURCE, sourceUrl: fetched.sourceUrl },
    fetchedAt: fetched.fetchedAt,
    contentSha256: fetched.contentSha256,
    httpStatus: fetched.httpStatus,
    etag: fetched.etag,
    outputDirectory,
    previousSnapshotPath,
  })

  if (report.merge && report.merge.reviewItems.length > 0) {
    console.warn(
      `Preserved ${report.merge.reviewItems.length} missing facts for review: ${report.merge.reviewItems.join(', ')}`,
    )
  }
  if (report.offerMerge && report.offerMerge.mergedOfferIds.length > 0) {
    console.warn(
      `Merged ${report.offerMerge.mergedOfferIds.length} duplicate free offers: ${report.offerMerge.mergedOfferIds.join(', ')}`,
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
