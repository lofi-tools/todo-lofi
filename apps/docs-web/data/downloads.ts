/**
 * Resolves the download links on the landing page and the install page to the
 * bundles of the current release.
 *
 * The bundles carry their version in the file name (`todo-lofi-<version>-…`),
 * so there is no fixed `releases/latest/download/<name>` URL to hard-code — the
 * asset name changes with every version. Rather than guess, the page asks the
 * GitHub releases API for the newest release that has an asset for the
 * visitor's platform and points the link at it. Every link already carries the
 * releases page as its `href`, so a failed request degrades to "pick the file
 * from the releases page" rather than a dead link.
 *
 * Lives outside `src/` for the same reason `data/landing.ts` does: Panda scans
 * every file under `src/` for style-shaped objects, and these records are data,
 * not styles.
 */

export const REPO_URL = 'https://github.com/lofi-tools/todo-lofi'
/** Every release, including the rolling pre-release the bundle workflow publishes. */
export const RELEASES_URL = `${REPO_URL}/releases`

const RELEASES_API = `https://api.github.com/repos/lofi-tools/todo-lofi/releases?per_page=30`

export type PlatformId = 'macos' | 'linux' | 'windows'
export type FormatId = 'dmg' | 'zip' | 'appimage' | 'deb' | 'tarball'

export interface Platform {
  id: PlatformId
  label: string
  requirement: string
}

export interface BundleFormat {
  id: FormatId
  /** The extension the asset name ends with the platform's arch segment before. */
  suffix: string
  label: string
  hint: string
}

export const PLATFORMS: Platform[] = [
  { id: 'macos', label: 'macOS', requirement: 'macOS 12 or newer' },
  { id: 'linux', label: 'Linux', requirement: 'x86_64 or aarch64' },
  { id: 'windows', label: 'Windows', requirement: 'Windows 10 or newer' },
]

/** The first format of each platform is the one the primary button downloads. */
export const FORMATS: Record<PlatformId, BundleFormat[]> = {
  macos: [
    {
      id: 'dmg',
      suffix: '.dmg',
      label: '.dmg',
      hint: 'Disk image: open it and drag todo-lofi into Applications.',
    },
    {
      id: 'zip',
      suffix: '.zip',
      label: '.zip',
      hint: 'Archive: unzip it and move todo-lofi.app into Applications.',
    },
  ],
  linux: [
    {
      id: 'appimage',
      suffix: '.AppImage',
      label: '.AppImage',
      hint: 'Portable: make it executable and run it. No install step.',
    },
    {
      id: 'deb',
      suffix: '.deb',
      label: '.deb',
      hint: 'Debian and Ubuntu: install it with your system installer.',
    },
    {
      id: 'tarball',
      suffix: '.tar.gz',
      label: '.tar.gz',
      hint: 'Tarball: unpack it and run usr/bin/todo-lofi.',
    },
  ],
  windows: [
    {
      id: 'zip',
      suffix: '.zip',
      label: '.zip',
      hint: 'Archive: unzip it and run todo-lofi.exe.',
    },
  ],
}

const PLATFORM_LABELS: Record<PlatformId, string> = {
  macos: 'macOS',
  linux: 'Linux',
  windows: 'Windows',
}

interface ReleaseAsset {
  name: string
  browser_download_url: string
}

interface Release {
  tag_name: string
  draft: boolean
  assets: ReleaseAsset[]
}

interface NavigatorWithUAData extends Navigator {
  userAgentData?: {
    platform?: string
    getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }>
  }
}

function detectPlatform(): PlatformId {
  const nav = navigator as NavigatorWithUAData
  const hinted = nav.userAgentData?.platform ?? nav.platform ?? ''
  const agent = navigator.userAgent
  if (/mac/i.test(hinted) || /mac os x/i.test(agent)) return 'macos'
  if (/win/i.test(hinted) || /windows/i.test(agent)) return 'windows'
  return 'linux'
}

async function detectArch(platform: PlatformId): Promise<string> {
  const nav = navigator as NavigatorWithUAData
  try {
    const values = await nav.userAgentData?.getHighEntropyValues?.(['architecture'])
    if (/arm/i.test(values?.architecture ?? '')) return 'aarch64'
    if (/x86|intel/i.test(values?.architecture ?? '')) return 'x86_64'
  } catch {
    // No high-entropy values: fall back to the per-platform default below.
  }
  // Apple Silicon is the safe default on macOS: an Intel Mac keeps reporting
  // "MacIntel" in its user agent, so the string cannot tell the two apart.
  return platform === 'macos' ? 'aarch64' : 'x86_64'
}

/**
 * The arch segment the packaging script wrote into the file name. Only macOS
 * differs: `uname -m` reports `arm64` there, while Linux reports `aarch64`.
 */
function archToken(platform: PlatformId, arch: string): string {
  return platform === 'macos' && arch === 'aarch64' ? 'arm64' : arch
}

function findAsset(
  releases: Release[],
  platform: PlatformId,
  format: FormatId,
  arch: string,
): { asset: ReleaseAsset; release: Release } | undefined {
  const spec = FORMATS[platform].find((entry) => entry.id === format)
  if (!spec) return undefined
  const wanted = archToken(platform, arch)
  const matches = (name: string, wantArch: boolean) => {
    const lower = name.toLowerCase()
    if (!name.endsWith(spec.suffix)) return false
    if (!lower.includes(`-${platform}-`)) return false
    return !wantArch || lower.includes(`-${wanted}`)
  }
  // The visitor's architecture first, then any architecture of the same
  // platform and format, so a machine without its own build still downloads
  // something that runs.
  for (const wantArch of [true, false]) {
    for (const release of releases) {
      const asset = release.assets.find((candidate) => matches(candidate.name, wantArch))
      if (asset) return { asset, release }
    }
  }
  return undefined
}

/** `todo-lofi-0.1.0-macos-arm64.zip` → `0.1.0`. */
function versionOf(assetName: string): string | undefined {
  return /^todo-lofi-(.+?)-(?:macos|linux|windows)-/.exec(assetName)?.[1]
}

async function loadReleases(): Promise<Release[]> {
  const response = await fetch(RELEASES_API, { headers: { Accept: 'application/vnd.github+json' } })
  if (!response.ok) throw new Error(`GitHub returned ${response.status}`)
  const releases = (await response.json()) as Release[]
  return releases.filter((release) => !release.draft)
}

/**
 * Wire up every `[data-download]` link on the page.
 *
 * Markup contract:
 * - `[data-download]` — an `<a>` to fill. `data-platform` is a platform id or
 *   `auto` (the currently selected platform); `data-format` is a format id and
 *   defaults to the platform's first format.
 * - `[data-download-option]` — a `<button>` that selects a platform.
 * - `[data-download-formats]` — a group shown only for its selected platform.
 * - `[data-download-detected]` — an element revealed only for the platform the
 *   visitor is actually on.
 * - `[data-download-primary]` / `[data-download-primary-label]` — the main
 *   button and the label that names the selected platform.
 * - `[data-download-name]` — an element inside a link, replaced with the
 *   resolved asset's file name.
 * - `[data-download-version]` — text replaced with the resolved version.
 */
export async function initDownloads(): Promise<void> {
  const links = Array.from(document.querySelectorAll<HTMLAnchorElement>('[data-download]'))
  if (links.length === 0) return

  const detected = detectPlatform()
  let selected: PlatformId = detected
  let arch = detected === 'macos' ? 'aarch64' : 'x86_64'
  let releases: Release[] | undefined

  const applySelection = () => {
    for (const option of document.querySelectorAll<HTMLElement>('[data-download-option]')) {
      const active = option.dataset.downloadOption === selected
      option.setAttribute('aria-pressed', String(active))
      // Present-or-absent, not "true"/"false": the selected style keys off
      // `&[data-selected]`, which an attribute set to "false" would still match.
      if (active) option.dataset.selected = 'true'
      else delete option.dataset.selected
    }
    for (const group of document.querySelectorAll<HTMLElement>('[data-download-formats]')) {
      group.hidden = group.dataset.downloadFormats !== selected
    }
    for (const mark of document.querySelectorAll<HTMLElement>('[data-download-detected]')) {
      mark.hidden = mark.dataset.downloadDetected !== detected
    }
  }

  const applyLinks = () => {
    let version: string | undefined
    let primaryResolved = false
    for (const link of links) {
      const raw = link.dataset.platform
      const platform: PlatformId = !raw || raw === 'auto' ? selected : (raw as PlatformId)
      if (!FORMATS[platform]) continue
      const format = (link.dataset.format as FormatId | undefined) ?? FORMATS[platform][0].id
      const found = releases ? findAsset(releases, platform, format, arch) : undefined
      if (!found) continue
      link.href = found.asset.browser_download_url
      if (!version) version = versionOf(found.asset.name)
      // A link may carry a `[data-download-name]` node that shows the file the
      // link actually points at, once that is known.
      const nameNode = link.querySelector<HTMLElement>('[data-download-name]')
      if (nameNode) nameNode.textContent = found.asset.name
      if (link.hasAttribute('data-download-primary')) primaryResolved = true
    }
    if (version) {
      for (const node of document.querySelectorAll<HTMLElement>('[data-download-version]')) {
        node.textContent = `v${version}`
      }
    }
    const label = document.querySelector<HTMLElement>('[data-download-primary-label]')
    if (label) {
      label.textContent = primaryResolved
        ? `Download for ${PLATFORM_LABELS[selected]}`
        : 'Browse all downloads'
    }
  }

  for (const option of document.querySelectorAll<HTMLElement>('[data-download-option]')) {
    option.addEventListener('click', () => {
      const next = option.dataset.downloadOption
      if (next === 'macos' || next === 'linux' || next === 'windows') {
        selected = next
        applySelection()
        applyLinks()
      }
    })
  }

  applySelection()
  try {
    arch = await detectArch(detected)
  } catch {
    // Keep the per-platform default.
  }
  try {
    releases = await loadReleases()
  } catch {
    // Leave every link on its `releases` page fallback.
  }
  applyLinks()
}
