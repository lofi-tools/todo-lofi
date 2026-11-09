import type { APIRoute } from 'astro'
import { loadCatalog } from '../lib/catalog'
import { buildHarnessRanking } from '../lib/ranking'

export const prerender = true

export const GET: APIRoute = async () => {
  const catalog = await loadCatalog()
  const ranking = buildHarnessRanking(catalog)

  return new Response(JSON.stringify(ranking, null, 2), {
    headers: {
      'content-type': 'application/json; charset=utf-8',
    },
  })
}
