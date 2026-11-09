import { css } from "styled-system/css";

/** Shared pieces for the homepage feature illustrations. */

/**
 * A mock window surface: the panel's three-stop falloff, dark enough to sit a
 * clear step below the page canvas, with a single shiny border and the lift off
 * the backdrop.
 *
 * The shine is the border itself. The panel's falloff is painted clipped to the
 * padding box, and behind it a conic gradient is clipped to the border box, so
 * the ring around the panel catches the light — brightest along the top edge
 * and dimming around the sides and bottom, the way a real bezel does. The
 * border is left transparent to let that ring through; the callers keep the
 * `hairline solid` border so it has a 1px ring to paint in. One border, no
 * stacked rings.
 */
export const windowSurface = css({
  borderColor: "transparent",
  backgroundOrigin: "padding-box, border-box",
  backgroundClip: "padding-box, border-box",
  backgroundImage:
    "linear-gradient(170deg, #1e1e24 0%, #16161a 45%, #101014 100%), conic-gradient(from 0deg at 50% 50%, rgba(255, 255, 255, 0.26) 0deg, rgba(255, 255, 255, 0.18) 38deg, rgba(255, 255, 255, 0.085) 84deg, rgba(255, 255, 255, 0.022) 148deg, rgba(255, 255, 255, 0.022) 212deg, rgba(255, 255, 255, 0.085) 276deg, rgba(255, 255, 255, 0.18) 322deg, rgba(255, 255, 255, 0.26) 360deg)",
  boxShadow: "inset 0 1px 0 0 rgba(255, 255, 255, 0.10), 0 24px 64px -12px rgba(0, 0, 0, 0.6)",
});

/**
 * The dark spotlight backdrop (also the `IntegrationSpotlight` wash): one
 * diagonal falloff from a faint indigo corner to a near-black floor.
 * Deliberately the inverse of a mock window — it is light-at-the-top while
 * windows run light-at-the-top too, but its darkest end sits below any window's
 * so backdrop and window never read as one surface.
 */
export const spotlightBackdrop = css({
  bg: "#0b0b0f",
  backgroundImage:
    "linear-gradient(140deg, rgba(94,106,210,0.14) 0%, #1b1b21 28%, #141419 62%, #0b0b0f 100%)",
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

/**
 * A one-line note set at the top of an illustration backdrop, before the
 * illustration: it explains the mock while the mock window itself stays a pure
 * screen replica.
 */
export const stageNote = css({
  fontSize: "xs",
  color: "fg.muted",
  lineHeight: "relaxed",
  maxW: "68ch",
});

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

/**
 * The shared soft illustration backdrop: container chrome plus the tone's
 * diagonal wash. One static class per tone, used by `IllustrationStage` and the
 * auto-switch tab pane alike, so the wash never drifts between copies.
 */
const stageBackdropBase = {
  position: "relative",
  overflow: "hidden",
  // An opaque base under the gradient: the tint stops are translucent, and on
  // their own would let the page show through the corner.
  bg: "surface.subtle",
} as const;

/**
 * The backdrop catalog: the washes an illustration stage can take, chosen with
 * `IllustrationStage`'s `tone`. The hues walk one arc — blue, indigo, violet,
 * purple, magenta — so adjacent stages shift in colour without leaving the
 * brand's range; indigo is the accent itself and the rest are its neighbours on
 * that arc. Pick a tone per illustration so neighbours don't repeat.
 *
 * Every stop keeps the stage's own hue: the two middle stops are the corner
 * colour at a lower alpha, not neutral surfaces, so the wash ramps out of the
 * tinted corner without the grey band a neutral mid colour would put across it.
 *
 * Static records rather than a function: Panda only emits `css()` calls it can
 * resolve at build time, and an interpolated value would silently drop the
 * whole gradient from the built CSS.
 */
export const stageBackdrops = {
  blue: css({
    ...stageBackdropBase,
    backgroundImage:
      "linear-gradient(140deg, rgba(70, 120, 225, 0.16) 0%, rgba(70, 120, 225, 0.08) 30%, rgba(70, 120, 225, 0.03) 62%, token(colors.surface.subtle) 100%)",
  }),
  indigo: css({
    ...stageBackdropBase,
    backgroundImage:
      "linear-gradient(140deg, token(colors.accent.subtle) 0%, rgba(94, 106, 210, 0.08) 30%, rgba(94, 106, 210, 0.03) 62%, token(colors.surface.subtle) 100%)",
  }),
  violet: css({
    ...stageBackdropBase,
    backgroundImage:
      "linear-gradient(140deg, rgba(116, 91, 196, 0.16) 0%, rgba(116, 91, 196, 0.08) 30%, rgba(116, 91, 196, 0.03) 62%, token(colors.surface.subtle) 100%)",
  }),
  purple: css({
    ...stageBackdropBase,
    backgroundImage:
      "linear-gradient(140deg, rgba(138, 76, 183, 0.16) 0%, rgba(138, 76, 183, 0.08) 30%, rgba(138, 76, 183, 0.03) 62%, token(colors.surface.subtle) 100%)",
  }),
  magenta: css({
    ...stageBackdropBase,
    backgroundImage:
      "linear-gradient(140deg, rgba(205, 88, 230, 0.16) 0%, rgba(205, 88, 230, 0.08) 30%, rgba(205, 88, 230, 0.03) 62%, token(colors.surface.subtle) 100%)",
  }),
} as const;

export type StageTone = keyof typeof stageBackdrops;

export type IllustrationFade = "bottom" | "sides" | "edges" | "none";

/**
 * The shared illustration fade: a mask gradient so the illustration melts
 * into its backdrop and reads as illustration, not a content card. Static
 * records for the same build-time reason as `stageBackdrops`.
 */
export const illustrationFades: Record<IllustrationFade, string> = {
  bottom: css({
    maskImage: "linear-gradient(to bottom, black 52%, black 72%, transparent 98%)",
    WebkitMaskImage: "linear-gradient(to bottom, black 52%, black 72%, transparent 98%)",
  }),
  sides: css({
    maskImage: "linear-gradient(to right, transparent 0%, black 7%, black 93%, transparent 100%)",
    WebkitMaskImage: "linear-gradient(to right, transparent 0%, black 7%, black 93%, transparent 100%)",
  }),
  edges: css({
    maskImage: "linear-gradient(to bottom, transparent 0%, black 10%, black 82%, transparent 100%)",
    WebkitMaskImage: "linear-gradient(to bottom, transparent 0%, black 10%, black 82%, transparent 100%)",
  }),
  none: "",
};
