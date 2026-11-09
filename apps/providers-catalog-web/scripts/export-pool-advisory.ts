import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { loadCatalog } from '../src/lib/catalog'
import { buildShepherdPoolCatalog } from '../src/lib/pool-export'

// The generated file is a local build artifact: it is not committed and the
// app treats a missing file as "no catalog", so a clean checkout still works.
const workspace = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..')
const target = process.env.SHEPHERD_POOL_CATALOG
  ? path.resolve(process.env.SHEPHERD_POOL_CATALOG)
  : path.join(workspace, '.cache/shepherd/pool-catalog-advisory.v2.json')

const catalog = await loadCatalog()
const payload = buildShepherdPoolCatalog(catalog)
await mkdir(path.dirname(target), { recursive: true })
await writeFile(target, `${JSON.stringify(payload, null, 2)}\n`)

const providers = payload.harnesses.flatMap((harness) => harness.providers).length
console.log(
  `Wrote ${payload.records.length} limit records and ${providers} provider choices to ${target}`,
)
