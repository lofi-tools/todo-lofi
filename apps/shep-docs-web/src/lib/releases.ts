// Read the newest Shepherd release from GitHub and pick the bundle that fits a
// visitor. The release workflow publishes `shepherd-v*` tags, so the resolver
// filters out other products' releases (taskstream publishes `v*` tags) and
// never treats an empty or malformed payload as a download.

export const REPO_SLUG = 'lofi-tools/todo-lofi'
export const REPO_URL = `https://github.com/${REPO_SLUG}`
export const RELEASES_API = `https://api.github.com/repos/${REPO_SLUG}/releases?per_page=30`
export const RELEASES_PAGE = `${REPO_URL}/releases`
export const TAG_PREFIX = 'shepherd-v'

export type Platform = 'macos' | 'linux' | 'windows' | 'unknown'
export type Arch = 'arm64' | 'x86_64' | undefined

export interface ReleaseAsset {
  name: string
  url: string
  size: number
}

export interface ShepherdRelease {
  tag: string
  version: string
  url: string
  publishedAt: string | null
  assets: ReleaseAsset[]
}

const BUNDLE = /\.(zip|deb|exe)$/
const METADATA = /^release-metadata-.*\.(json|sha256)$/

export function shepherdReleaseFrom(payload: unknown): ShepherdRelease | null {
  if (!Array.isArray(payload)) return null

  for (const entry of payload) {
    if (!entry || typeof entry !== 'object') continue
    const record = entry as Record<string, unknown>
    const tag = typeof record.tag_name === 'string' ? record.tag_name : ''
    if (!tag.startsWith(TAG_PREFIX)) continue

    const assets = Array.isArray(record.assets)
      ? record.assets.flatMap((asset): ReleaseAsset[] => {
          if (!asset || typeof asset !== 'object') return []
          const item = asset as Record<string, unknown>
          const name = typeof item.name === 'string' ? item.name : ''
          const url = typeof item.browser_download_url === 'string' ? item.browser_download_url : ''
          if (!name || !url) return []
          return [{ name, url, size: typeof item.size === 'number' ? item.size : 0 }]
        })
      : []

    // A tag with no downloadable files is not a download page.
    if (assets.length === 0) return null

    const url =
      typeof record.html_url === 'string' ? record.html_url : `${RELEASES_PAGE}/tag/${tag}`
    return {
      tag,
      version: tag.slice(TAG_PREFIX.length),
      url,
      publishedAt: typeof record.published_at === 'string' ? record.published_at : null,
      assets,
    }
  }

  return null
}

export function bundleAssets(release: ShepherdRelease): ReleaseAsset[] {
  return release.assets.filter((asset) => BUNDLE.test(asset.name) && !METADATA.test(asset.name))
}

export function metadataAssets(release: ShepherdRelease): ReleaseAsset[] {
  return release.assets.filter((asset) => METADATA.test(asset.name))
}

export function platformFrom(
  platform: string | null | undefined,
  userAgent: string | null | undefined,
): Platform {
  const hint = (platform ?? '').toLowerCase()
  if (hint.includes('mac')) return 'macos'
  if (hint.includes('win')) return 'windows'
  if (hint.includes('linux') && !hint.includes('android')) return 'linux'

  const agent = (userAgent ?? '').toLowerCase()
  if (agent.includes('android')) return 'unknown'
  if (agent.includes('windows')) return 'windows'
  if (agent.includes('mac os') || agent.includes('macintosh')) return 'macos'
  if (agent.includes('linux')) return 'linux'
  return 'unknown'
}

export function archFrom(
  architecture: string | null | undefined,
  platform: string | null | undefined,
): Arch {
  const value = (architecture ?? '').toLowerCase()
  if (value.includes('arm') || value.includes('aarch64')) return 'arm64'
  if (/x86|amd64|x64|intel/.test(value)) return 'x86_64'

  const hint = (platform ?? '').toLowerCase()
  if (hint.includes('arm') || hint.includes('aarch64')) return 'arm64'
  if (/x86|amd64|x64|intel/.test(hint)) return 'x86_64'
  return undefined
}

/** The bundles that fit one platform, narrowed to an architecture when known. */
export function assetsFor(
  release: ShepherdRelease,
  platform: Platform,
  arch: Arch,
): ReleaseAsset[] {
  const bundles = bundleAssets(release)
  if (platform === 'unknown') return bundles
  return bundles.filter(
    (asset) => asset.name.includes(`-${platform}-`) && (arch == null || asset.name.includes(`-${arch}`)),
  )
}
