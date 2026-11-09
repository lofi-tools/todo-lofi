import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

export const SCHEMA_VERSION = 1
export const SCHEMA_SQL_PATH = fileURLToPath(new URL('../schema/0001_catalog.sql', import.meta.url))

export function loadSchemaSql(): string {
  return readFileSync(SCHEMA_SQL_PATH, 'utf8')
}
