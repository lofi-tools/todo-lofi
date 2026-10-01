/**
 * Resolves the download links on the downloads page and the install page to
 * the bundles of the current release.
 *
 * The bundles carry their version in the file name (`<name>-<version>-…`),
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

// The repository has not moved yet: the code is renamed, the repo is not.
// Point every repo link (header, footer, source URLs) and the releases API at
// the old name until the rename happens on GitHub.
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

export interface Arch {
  /** The internal arch id (`aarch64` maps to the `arm64` file-name token on macOS). */
  id: string
  label: string
  /** Whether CI builds this arch (see `.github/workflows/bundle.yml`). */
  available: boolean
}

/**
 * Every OS/arch combination the page shows, in display order. CI builds one
 * arch per OS today (macOS on Apple Silicon, Linux and Windows on x86_64);
 * unavailable combinations render grayed out. Flip a flag (and add a runner)
 * when a new arch ships and both the site and the dropdown pick it up.
 */
export const ARCHES: Record<PlatformId, Arch[]> = {
  macos: [
    { id: 'aarch64', label: 'arm64', available: true },
    { id: 'x86_64', label: 'x86_64', available: false },
  ],
  linux: [
    { id: 'x86_64', label: 'x86_64', available: true },
    { id: 'aarch64', label: 'aarch64', available: false },
  ],
  windows: [
    { id: 'x86_64', label: 'x86_64', available: true },
    { id: 'aarch64', label: 'arm64', available: false },
  ],
}

/** The arch a single-button surface (e.g. the homepage dropdown) downloads. */
export function primaryArch(platform: PlatformId): Arch {
  return ARCHES[platform].find((arch) => arch.available) ?? ARCHES[platform][0]
}

/** The first format of each platform is the one the primary button downloads. */
export const FORMATS: Record<PlatformId, BundleFormat[]> = {
  macos: [
    {
      id: 'dmg',
      suffix: '.dmg',
      label: '.dmg',
      hint: 'Disk image: open it and drag taskstream into Applications.',
    },
    {
      id: 'zip',
      suffix: '.zip',
      label: '.zip',
      hint: 'Archive: unzip it and move taskstream.app into Applications.',
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
      hint: 'Tarball: unpack it and run usr/bin/taskstream.',
    },
  ],
  windows: [
    {
      id: 'zip',
      suffix: '.zip',
      label: '.zip',
      hint: 'Archive: unzip it and run taskstream.exe.',
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

/** The OS the visitor is on, from user-agent hints with a UA-string fallback. */
export function detectPlatform(): PlatformId {
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

/** `taskstream-0.1.0-macos-arm64.zip` (or the old `todo-lofi-…` name) → `0.1.0`. */
function versionOf(assetName: string): string | undefined {
  return /^(?:taskstream|todo-lofi)-(.+?)-(?:macos|linux|windows)-/.exec(assetName)?.[1]
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
 *   `auto` (the platform the visitor is on); `data-arch` pins an architecture
 *   id and defaults to the visitor's; `data-format` is a format id and
 *   defaults to the platform's first format.
 * - `[data-download-detected]` — an element revealed only for the platform the
 *   visitor is actually on.
 * - `[data-download-primary]` / `[data-download-primary-label]` — the main
 *   button and the label that names its platform.
 * - `[data-download-primary-format]` — text replaced with the install method
 *   the primary button points at, and `[data-download-primary-arch]` with the
 *   architecture it was detected on. Both stay hidden until the button has an
 *   asset to point at, so neither claims anything the link does not do.
 * - `[data-download-name]` — an element inside a link, replaced with the
 *   resolved asset's file name.
 * - `[data-download-version]` — text replaced with the resolved version.
 */
export async function initDownloads(): Promise<void> {
  const links = Array.from(document.querySelectorAll<HTMLAnchorElement>('[data-download]'))
  if (links.length === 0) return

  // Nothing on the page switches the platform: a build for another machine is
  // reached from the file list, not from a picker.
  const platform = detectPlatform()
  let arch = platform === 'macos' ? 'aarch64' : 'x86_64'
  let releases: Release[] | undefined
  let primaryResolved = false

  const applyDetected = () => {
    for (const mark of document.querySelectorAll<HTMLElement>('[data-download-detected]')) {
      mark.hidden = mark.dataset.downloadDetected !== platform
    }
  }

  // Name the visitor's platform as soon as it is known. `releases` is the
  // readiness flag: until the request comes back the primary button already
  // points at the releases page, so naming the platform is honest; once it
  // comes back with no matching asset, the label says what the link does.
  const applyPrimaryLabel = () => {
    const text =
      !releases || primaryResolved
        ? `Download for ${PLATFORM_LABELS[platform]}`
        : 'Browse all downloads'
    for (const label of document.querySelectorAll<HTMLElement>('[data-download-primary-label]')) {
      label.textContent = text
    }
  }

  // The install method and the machine's architecture only mean something
  // once the primary link has resolved to a file.
  const applyPrimaryDetails = () => {
    const archLabel = ARCHES[platform].find((candidate) => candidate.id === arch)?.label ?? arch
    const details: Array<[string, string]> = [
      ['[data-download-primary-format]', FORMATS[platform][0].label],
      ['[data-download-primary-arch]', archLabel],
    ]
    for (const [selector, text] of details) {
      for (const node of document.querySelectorAll<HTMLElement>(selector)) {
        node.textContent = text
        node.hidden = !primaryResolved
      }
    }
  }

  const applyLinks = () => {
    let version: string | undefined
    primaryResolved = false
    for (const link of links) {
      const raw = link.dataset.platform
      const target: PlatformId = !raw || raw === 'auto' ? platform : (raw as PlatformId)
      if (!FORMATS[target]) continue
      const format = (link.dataset.format as FormatId | undefined) ?? FORMATS[target][0].id
      const wantArch = link.dataset.arch ?? arch
      const found = releases ? findAsset(releases, target, format, wantArch) : undefined
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
    applyPrimaryLabel()
    applyPrimaryDetails()
  }

  applyDetected()
  applyPrimaryLabel()
  applyPrimaryDetails()
  try {
    arch = await detectArch(platform)
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
