import { css } from "styled-system/css";

/** Shared pieces for the homepage feature illustrations. */

/**
 * The ambient backdrop from the reference screens: a few soft radial lifts
 * over a dark falloff, plus a faint indigo tint in the top corner. Used by the
 * outer illustration frames, with the windows and cards layered on top.
 */
export const backdropSurface = css({
  backgroundImage: [
    "radial-gradient(130% 100% at 62% 28%, token(colors.surface.hover) 0%, transparent 64%)",
    "radial-gradient(150% 110% at 50% 118%, token(colors.surface.subtle) 0%, transparent 60%)",
    "radial-gradient(105% 85% at 12% 22%, token(colors.surface.elevated) 0%, transparent 62%)",
    "radial-gradient(85% 62% at 90% 8%, token(colors.accent.subtle) 0%, transparent 62%)",
    "linear-gradient(180deg, token(colors.surface.subtle) 0%, token(colors.canvas) 55%, token(colors.canvas) 100%)",
  ].join(", "),
});

/**
 * A mock window surface: the reference's panel gradient (a three-stop falloff
 * with a light radial sheen at the top), lifted off the backdrop by a shadow.
 */
export const windowSurface = css({
  backgroundImage: [
    "radial-gradient(120% 90% at 62% 0%, token(colors.surface.hover) 0%, transparent 60%)",
    "radial-gradient(80% 60% at 90% 6%, token(colors.accent.subtle) 0%, transparent 62%)",
    "linear-gradient(170deg, token(colors.surface.elevated) 0%, token(colors.surface.subtle) 45%, token(colors.canvas) 100%)",
  ].join(", "),
  boxShadow: "xl",
});

/** The inset body of a mock window, a step darker than its chrome. */
export const windowBody = css({
  backgroundImage:
    "linear-gradient(180deg, token(colors.surface.subtle) 0%, token(colors.canvas) 100%)",
});

/**
 * The header of a mock window: dots, a title, and a hairline. It paints no
 * background of its own, so the window's own gradient runs through the header
 * and the whole panel reads as one surface.
 */
export const windowBar = css({
  display: "flex",
  alignItems: "center",
  gap: "1.5",
  flexShrink: "0",
  px: "3",
  h: "7",
  borderBottomWidth: "hairline",
  borderBottomStyle: "solid",
  borderBottomColor: "border.subtle",
});

/** A mock task row: checkbox, label, optional trailing detail. */
export const row = css({
  display: "flex",
  alignItems: "center",
  gap: "2",
  px: "2",
  py: "1.5",
  borderRadius: "md",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border.strong",
  backgroundImage:
    "linear-gradient(160deg, token(colors.surface.hover) 0%, token(colors.surface.elevated) 55%, token(colors.surface.subtle) 100%)",
});

export const checkbox = css({
  w: "3",
  h: "3",
  flexShrink: "0",
  borderRadius: "xs",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border.strong",
});

export const label = css({ fontSize: "xs", color: "fg.default", whiteSpace: "nowrap" });

/** A row's leading and trailing markers, e.g. a pane name and a status chip. */
export const rowHead = css({
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "3",
  flexWrap: "wrap",
});

export const column = css({ display: "flex", flexDirection: "column", gap: "2" });

/** A mono eyebrow: short, uppercase, for labels like "agent" or "trip". */
export const mono = css({
  fontFamily: "mono",
  fontSize: "2xs",
  color: "fg.subtle",
  textTransform: "uppercase",
  letterSpacing: "wide",
});

/** A short sentence under an illustration, in normal case. */
export const caption = css({ fontSize: "xs", color: "fg.muted", lineHeight: "relaxed" });

export const chip = css({
  fontFamily: "mono",
  fontSize: "2xs",
  px: "1.5",
  py: "0.5",
  borderRadius: "sm",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border.strong",
  backgroundImage:
    "linear-gradient(180deg, token(colors.surface.active) 0%, token(colors.surface.hover) 100%)",
  color: "fg.muted",
  whiteSpace: "nowrap",
});

/**
 * A full-bleed overlay for connector threads; `threads.ts` draws the paths.
 * Hidden when the layout stacks, since a thread would loop back across the app.
 */
export const threadLayer = css({
  position: "absolute",
  inset: "0",
  w: "full",
  h: "full",
  overflow: "visible",
  pointerEvents: "none",
  zIndex: "raised",
  display: { base: "none", md: "block" },
});

export const threadPath = css({
  fill: "none",
  stroke: "accent.text",
  strokeWidth: "1.5",
  strokeLinecap: "round",
  opacity: "0.55",
});

export const threadNode = css({ fill: "accent.text", opacity: "0.75" });

export const dot = css({ w: "2", h: "2", borderRadius: "full", bg: "border.strong" });

/** A small mono heading over a pane, e.g. "tags" or "today". */
export const paneLabel = css({
  fontFamily: "mono",
  fontSize: "2xs",
  textTransform: "uppercase",
  letterSpacing: "wide",
  color: "fg.subtle",
  px: "1",
  mb: "0.5",
});
