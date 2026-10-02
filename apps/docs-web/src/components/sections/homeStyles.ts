import { css } from "styled-system/css";

/** Shared layout primitives for the homepage sections. */

export const grid = css({
  display: "grid",
  gridTemplateColumns: { base: "1fr", md: "repeat(2, minmax(0, 1fr))" },
  gap: "4",
});

export const card = css({
  display: "flex",
  flexDirection: "column",
  gap: "3",
  p: { base: "5", md: "6" },
  borderRadius: "lg",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border",
  bg: "surface.subtle",
});

export const cardTitle = css({
  display: "flex",
  alignItems: "center",
  gap: "3",
  flexWrap: "wrap",
});

export const cardHeading = css({ fontSize: "xl", fontWeight: "semibold", color: "fg.default" });

export const cardBody = css({ fontSize: "sm", color: "fg.muted", lineHeight: "relaxed", m: "0" });

export const cardLink = css({
  display: "inline-flex",
  alignItems: "center",
  mt: "auto",
  pt: "1",
  fontSize: "sm",
  fontWeight: "medium",
  color: "accent.text",
  _hover: { color: "accent.hover" },
  _focusVisible: { outline: "none", boxShadow: "focus" },
});

export const panel = css({
  p: "4",
  borderRadius: "lg",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border",
  bg: "surface.subtle",
});

export const band = css({
  display: "flex",
  flexDirection: { base: "column", md: "row" },
  alignItems: { base: "flex-start", md: "center" },
  justifyContent: "space-between",
  gap: "5",
  p: { base: "6", md: "8" },
  borderRadius: "xl",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border",
  bg: "surface.subtle",
});

export const bandText = css({ display: "flex", flexDirection: "column", gap: "1" });

export const bandLink = css({
  display: "inline-flex",
  alignItems: "center",
  h: "10",
  px: "5",
  borderRadius: "sm",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border",
  fontSize: "md",
  color: "fg.default",
  whiteSpace: "nowrap",
  _hover: { bg: "surface.hover" },
  _focusVisible: { outline: "none", boxShadow: "focus" },
});

export const inlineLink = css({
  color: "fg.default",
  textDecoration: "underline",
  textUnderlineOffset: "2px",
  _hover: { color: "accent.text" },
  _focusVisible: { outline: "none", boxShadow: "focus" },
});

/**
 * Feature items laid out as a grid whose 1px gaps read as hairlines between
 * the items; the items carry the page background so only the gaps show.
 */
const itemGrid = {
  display: "grid",
  gap: "1px",
  bg: "border.subtle",
};

export const reasonsGrid = css({
  ...itemGrid,
  gridTemplateColumns: { base: "1fr", md: "repeat(3, minmax(0, 1fr))" },
});

/**
 * Sub-sections of one feature, delimited by the same hairline gaps: the page
 * background shows through the 1px gutter, so the rule is drawn by the grid
 * rather than added as a border.
 */
export const subGrid = css({
  ...itemGrid,
  gridTemplateColumns: { base: "1fr", lg: "repeat(2, minmax(0, 1fr))" },
  // The items stretch to the row height: the grid's own colour is the gutter
  // between them, so a short item would otherwise leave it exposed below.
});

/** Vertical auto-advancing tabs: a tab column beside the active panel. */
export const tabsLayout = css({
  display: "grid",
  gridTemplateColumns: { base: "1fr", lg: "minmax(0, 300px) minmax(0, 1fr)" },
  gap: { base: "5", lg: "10" },
  alignItems: "start",
});

export const tabsColumn = css({ display: "flex", flexDirection: "column", gap: "4" });

export const tabList = css({
  display: "flex",
  flexDirection: "column",
  gap: "1px",
  bg: "border.subtle",
});

export const tab = css({
  display: "flex",
  flexDirection: "column",
  w: "100%",
  textAlign: "left",
  bg: "canvas",
  cursor: "pointer",
  borderLeftWidth: "2px",
  borderLeftStyle: "solid",
  borderLeftColor: "transparent",
  _hover: { bg: "surface.hover" },
  _focusVisible: { outline: "none", boxShadow: "focus" },
  "&[data-active]": { bg: "surface.subtle", borderLeftColor: "accent.text" },
  "&[data-active] [data-tab-title]": { color: "fg.default" },
  // The active tab grows to show its description, and each tab carries its own
  // progress track so the bar always sits directly under the tab it belongs to.
  "&[data-active] [data-tab-description]": { display: "block" },
  "&[data-active] [data-tab-progress-track]": { display: "block" },
});

/** The tab's own grid, wrapped in the button so the progress track can sit
 * flush against the button's bottom edge instead of inside this padding. */
export const tabContent = css({
  display: "grid",
  gridTemplateColumns: "auto 1fr",
  alignItems: "baseline",
  columnGap: "3",
  rowGap: "1",
  px: "4",
  py: "3",
});

export const tabIndex = css({
  gridRow: "span 2",
  fontFamily: "mono",
  fontSize: "xs",
  color: "fg.subtle",
});

// The tab is a button, and buttons do not select their text by default; the
// copy inside one is worth quoting, so opt each part back in.
export const tabTitle = css({ fontSize: "md", fontWeight: "medium", color: "fg.muted", userSelect: "text" });

export const tabSubtitle = css({ fontSize: "xs", color: "fg.subtle", userSelect: "text" });

export const tabDescription = css({
  display: "none",
  // Column two, so the description lines up with the title and subtitle
  // rather than with the index gutter.
  gridColumn: "2",
  m: "0",
  fontSize: "sm",
  color: "fg.muted",
  lineHeight: "relaxed",
  userSelect: "text",
});

export const tabProgressTrack = css({
  display: "none",
  w: "100%",
  h: "2px",
  bg: "border.subtle",
  overflow: "hidden",
});

export const tabProgressBar = css({
  display: "block",
  w: "100%",
  h: "2px",
  // A dimmer indigo than the `accent.text` selected-tab indicator, so the
  // timed fill reads as secondary to the selection it advances.
  bg: "accent",
  opacity: "0.5",
  transformOrigin: "left",
  transform: "scaleX(0)",
});

export const tabPanel = css({
  display: "flex",
  flexDirection: "column",
  gap: "4",
  p: { base: "5", md: "6" },
  borderRadius: "lg",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border.subtle",
  bg: "surface.subtle",
  "&[hidden]": { display: "none" },
  _focusVisible: { outline: "none", boxShadow: "focus" },
});

export const item = css({
  display: "flex",
  flexDirection: "column",
  gap: "3",
  p: { base: "5", md: "6" },
  bg: "canvas",
});

export const itemHeading = css({ fontSize: "lg", fontWeight: "semibold", color: "fg.default", m: "0" });

export const itemSubtitle = css({ fontSize: "sm", fontWeight: "medium", color: "accent.text", m: "0" });

export const glyph = css({
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  w: "10",
  h: "10",
  borderRadius: "sm",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border",
  bg: "surface.subtle",
  color: "accent.text",
});

/** Width of the centered content column every homepage row aligns to. */
export const CONTENT_WIDTH = "1104px";

/**
 * The homepage layout grid. Columns are `global margin | content | global
 * margin`, so a row placed with `blueprintContent` sits inside the page
 * margin while `blueprintBleed` spans the full viewport. Without `display:
 * grid` the rows fall back to a centered max-width column, so the page still
 * reads even where grid is unavailable.
 */
export const blueprintGrid = css({
  display: "grid",
  gridTemplateColumns: {
    base: "minmax(0, 1fr)",
    md: `minmax(0, 1fr) minmax(0, ${CONTENT_WIDTH}) minmax(0, 1fr)`,
  },
  // Full-bleed dividers are 100vw wide; clip the sub-pixel overflow a
  // scrollbar would otherwise turn into a horizontal scroll.
  overflowX: "clip",
});

/** A row inside the global margin, aligned to the content column. */
export const blueprintContent = css({
  gridColumn: { base: "1", md: "2" },
  w: "100%",
  maxW: CONTENT_WIDTH,
  mx: "auto",
  minW: "0",
});

/** A row that bleeds past the global margin to the viewport edges. */
export const blueprintBleed = css({ gridColumn: "1 / -1", minW: "0" });

/**
 * Full-bleed hairline above a row. The row is centered, so a 100vw
 * pseudo-element centred on it reaches both viewport edges.
 */
export const blueprintDivider = css({
  position: "relative",
  _before: {
    content: '""',
    position: "absolute",
    top: "0",
    left: "50%",
    transform: "translateX(-50%)",
    w: "100vw",
    h: "1px",
    bg: "border.subtle",
    pointerEvents: "none",
  },
});

/**
 * The vertical blueprint rules at the content-column edges. Rendered once per
 * page as a fixed overlay so the lines run unbroken through the header, hero,
 * sections, and footer; `100%` tracks the content column at every width.
 */
export const blueprintRules = css({
  position: "fixed",
  top: "0",
  bottom: "0",
  left: "50%",
  transform: "translateX(-50%)",
  w: "100%",
  maxW: CONTENT_WIDTH,
  borderLeftWidth: "hairline",
  borderRightWidth: "hairline",
  // Without an explicit zero the other sides default to `medium` (3px).
  borderTopWidth: "0",
  borderBottomWidth: "0",
  borderStyle: "solid",
  borderColor: "border.subtle",
  pointerEvents: "none",
  zIndex: "overlay",
  display: { base: "none", md: "block" },
});
