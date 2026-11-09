import type { APIRoute } from 'astro'
import { buildAgentPoolAdvisory } from '../lib/agent-pool-advisory'
import { loadCatalog } from '../lib/catalog'

export const prerender = true

export const GET: APIRoute = async () => {
  const catalog = await loadCatalog()
  const advisory = buildAgentPoolAdvisory(catalog)

  return new Response(JSON.stringify(advisory, null, 2), {
    headers: {
      'content-type': 'application/json; charset=utf-8',
    },
  })
}
