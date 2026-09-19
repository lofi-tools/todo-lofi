/**
 * Headless layer (spec §4.1).
 *
 * The unstyled, accessible behavior primitives. Components in the styled
 * layer import from here rather than from `@ark-ui/solid` directly, so the
 * behavior dependency stays swappable and the layer boundary is truthful.
 */
export {
  Accordion,
  Avatar,
  Carousel,
  Checkbox,
  Combobox,
  Dialog,
  Field,
  HoverCard,
  Menu,
  Popover,
  Progress,
  RadioGroup,
  SegmentGroup,
  Select,
  Slider,
  Switch,
  Tabs,
  Toast,
  Tooltip,
} from '@ark-ui/solid'

export type { DialogRootProps } from '@ark-ui/solid'
