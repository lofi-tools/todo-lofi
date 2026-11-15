/**
 * Freebuff's brand mark, shared by the composer's harness picker and the
 * Terminals tab so both show the same icon.
 */
export function FreebuffMark(props: { size?: number }) {
  const size = () => props.size ?? 14
  return (
    <svg class="agent-mark" width={size()} height={size()} viewBox="0 0 96.81 96.81" aria-hidden="true">
      <rect width="96.81" height="96.81" rx="18" fill="#2C7A40" />
      <g fill="#fff">
        <rect x="24.67" y="24.67" width="47.47" height="11.87" />
        <rect x="24.67" y="36.54" width="11.87" height="35.61" />
        <rect x="40.06" y="39.98" width="23.74" height="11.87" />
        <rect x="40.06" y="51.85" width="11.87" height="20.3" />
      </g>
    </svg>
  )
}
