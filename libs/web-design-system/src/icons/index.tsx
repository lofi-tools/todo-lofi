/**
 * Placeholder icon set.
 *
 * Linear's icons are custom and cannot be reproduced (spec §11.2). These are
 * simple stroke icons behind a shared `IconProps` API; swapping in another set
 * later means replacing this file, not every component.
 */
import type { JSX } from 'solid-js'
import { splitProps } from 'solid-js'

export type IconProps = Omit<JSX.SvgSVGAttributes<SVGSVGElement>, 'children'> & {
  size?: number | string
}

function Svg(props: IconProps & { children: JSX.Element }): JSX.Element {
  const [local, rest] = splitProps(props, ['size', 'children'])
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={local.size ?? 16}
      height={local.size ?? 16}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.75"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      {...rest}
    >
      {local.children}
    </svg>
  )
}

export const ArrowRight = (props: IconProps) => (
  <Svg {...props}>
    <path d="M5 12h14" />
    <path d="m12 5 7 7-7 7" />
  </Svg>
)

export const ArrowUpRight = (props: IconProps) => (
  <Svg {...props}>
    <path d="M7 17 17 7" />
    <path d="M7 7h10v10" />
  </Svg>
)

export const Check = (props: IconProps) => (
  <Svg {...props}>
    <path d="m20 6-11 11-5-5" />
  </Svg>
)

export const ChevronDown = (props: IconProps) => (
  <Svg {...props}>
    <path d="m6 9 6 6 6-6" />
  </Svg>
)

export const ChevronRight = (props: IconProps) => (
  <Svg {...props}>
    <path d="m9 18 6-6-6-6" />
  </Svg>
)

export const ChevronLeft = (props: IconProps) => (
  <Svg {...props}>
    <path d="m15 18-6-6 6-6" />
  </Svg>
)

export const Search = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="11" cy="11" r="7" />
    <path d="m20 20-3.5-3.5" />
  </Svg>
)

export const Settings = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
  </Svg>
)

export const Bell = (props: IconProps) => (
  <Svg {...props}>
    <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
    <path d="M10.3 21a2 2 0 0 0 3.4 0" />
  </Svg>
)

export const Inbox = (props: IconProps) => (
  <Svg {...props}>
    <path d="M22 12h-6l-2 3h-4l-2-3H2" />
    <path d="M5.5 5.5 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.5-6.5A2 2 0 0 0 16.8 4H7.2a2 2 0 0 0-1.7 1.5Z" />
  </Svg>
)

export const Layers = (props: IconProps) => (
  <Svg {...props}>
    <path d="m12 2 9 5-9 5-9-5 9-5Z" />
    <path d="m3 12 9 5 9-5" />
    <path d="m3 17 9 5 9-5" />
  </Svg>
)

export const Box = (props: IconProps) => (
  <Svg {...props}>
    <path d="M21 8 12 3 3 8v8l9 5 9-5V8Z" />
    <path d="m3 8 9 5 9-5" />
    <path d="M12 13v8" />
  </Svg>
)

export const Roadmap = (props: IconProps) => (
  <Svg {...props}>
    <path d="M4 4v16" />
    <path d="M4 5h11l-1.5 3L15 11H4" />
    <path d="M4 13h8l-1 2 1 3H4" />
  </Svg>
)

export const Circle = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="9" />
  </Svg>
)

export const CircleDot = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="9" />
    <circle cx="12" cy="12" r="3.5" fill="currentColor" stroke="none" />
  </Svg>
)

export const CircleHalf = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 3v18a9 9 0 0 0 0-18Z" fill="currentColor" stroke="none" />
  </Svg>
)

export const X = (props: IconProps) => (
  <Svg {...props}>
    <path d="M18 6 6 18" />
    <path d="m6 6 12 12" />
  </Svg>
)

export const Menu = (props: IconProps) => (
  <Svg {...props}>
    <path d="M4 6h16" />
    <path d="M4 12h16" />
    <path d="M4 18h16" />
  </Svg>
)

export const Sun = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M19.1 4.9l-1.4 1.4M6.3 17.7l-1.4 1.4" />
  </Svg>
)

export const Moon = (props: IconProps) => (
  <Svg {...props}>
    <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z" />
  </Svg>
)

export const Command = (props: IconProps) => (
  <Svg {...props}>
    <path d="M15 6a3 3 0 1 1 3 3h-3V6ZM9 6a3 3 0 1 0-3 3h3V6ZM9 18a3 3 0 1 1-3-3h3v3ZM15 18a3 3 0 1 0 3-3h-3v3Z" />
    <path d="M9 9h6v6H9z" />
  </Svg>
)

export const User = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="8" r="4" />
    <path d="M4 21a8 8 0 0 1 16 0" />
  </Svg>
)

export const Users = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="9" cy="8" r="3.5" />
    <path d="M2.5 20a6.5 6.5 0 0 1 13 0" />
    <path d="M16 5.2a3.5 3.5 0 0 1 0 6.6" />
    <path d="M18 14.5a6.5 6.5 0 0 1 3.5 5.5" />
  </Svg>
)

export const Calendar = (props: IconProps) => (
  <Svg {...props}>
    <rect x="3" y="5" width="18" height="16" rx="2" />
    <path d="M3 10h18M8 3v4M16 3v4" />
  </Svg>
)

export const Clock = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 7v5l3 2" />
  </Svg>
)

export const Filter = (props: IconProps) => (
  <Svg {...props}>
    <path d="M3 5h18l-7 8v6l-4-2v-4L3 5Z" />
  </Svg>
)

export const MoreHorizontal = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="5" cy="12" r="1.5" fill="currentColor" stroke="none" />
    <circle cx="12" cy="12" r="1.5" fill="currentColor" stroke="none" />
    <circle cx="19" cy="12" r="1.5" fill="currentColor" stroke="none" />
  </Svg>
)

export const GitPullRequest = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="6" cy="6" r="3" />
    <circle cx="6" cy="18" r="3" />
    <path d="M6 9v6" />
    <circle cx="18" cy="18" r="3" />
    <path d="M18 15V8a3 3 0 0 0-3-3h-3" />
    <path d="m14 2-2 3 2 3" />
  </Svg>
)

export const Code = (props: IconProps) => (
  <Svg {...props}>
    <path d="m9 18-6-6 6-6" />
    <path d="m15 6 6 6-6 6" />
  </Svg>
)

export const Play = (props: IconProps) => (
  <Svg {...props}>
    <path d="M6 4.5v15l13-7.5-13-7.5Z" />
  </Svg>
)

export const Sparkles = (props: IconProps) => (
  <Svg {...props}>
    <path d="M12 3l1.6 4.4L18 9l-4.4 1.6L12 15l-1.6-4.4L6 9l4.4-1.6L12 3Z" />
    <path d="M18.5 15l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2Z" />
  </Svg>
)

export const Link = (props: IconProps) => (
  <Svg {...props}>
    <path d="M10 13a5 5 0 0 0 7.5.5l2-2A5 5 0 0 0 12.5 4.5l-1 1" />
    <path d="M14 11a5 5 0 0 0-7.5-.5l-2 2A5 5 0 0 0 11.5 19.5l1-1" />
  </Svg>
)

export const AlertCircle = (props: IconProps) => (
  <Svg {...props}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 8v5" />
    <path d="M12 16.5h.01" />
  </Svg>
)

export const TrendingUp = (props: IconProps) => (
  <Svg {...props}>
    <path d="m3 17 6-6 4 4 8-8" />
    <path d="M15 7h6v6" />
  </Svg>
)

export const Zap = (props: IconProps) => (
  <Svg {...props}>
    <path d="M13 2 4 14h6l-1 8 9-12h-6l1-8Z" />
  </Svg>
)

export const BarChart = (props: IconProps) => (
  <Svg {...props}>
    <path d="M4 20V10M10 20V4M16 20v-7M22 20H2" />
  </Svg>
)

/** Neutral placeholder mark. Deliberately not any real company's logo. */
export const LogoMark = (props: IconProps) => (
  <Svg {...props}>
    <rect x="3" y="3" width="18" height="18" rx="5" />
    <path d="M8 12h8" />
    <path d="M12 8v8" />
  </Svg>
)

export type IconComponent = (props: IconProps) => JSX.Element
