import { css } from "styled-system/css";

/** Shared pieces for the homepage feature illustrations. */

/**
 * The ambient backdrop from the reference screens: a few soft radial lifts
 * over a dark falloff, plus a faint indigo tint in the top corner. Used by the
 * outer illustration frames, with the windows and cards layered on top.
 */
export const backdropSurface = css({
  backgroundImage: [
    "radial-gradient(130% 100% at 18% 8%, token(colors.surface.active) 0%, transparent 64%)",
    "radial-gradient(150% 110% at 50% 118%, token(colors.surface.subtle) 0%, transparent 60%)",
    "radial-gradient(105% 85% at 0% 0%, token(colors.surface.elevated) 0%, transparent 62%)",
    "radial-gradient(85% 62% at 12% 0%, token(colors.accent.subtle) 0%, transparent 62%)",
    "linear-gradient(135deg, token(colors.surface.hover) 0%, token(colors.canvas) 55%, token(colors.canvas) 100%)",
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

/**
 * The dark spotlight backdrop (also the `IntegrationSpotlight` wash): deep
 * radial lifts over a near-black falloff. Deliberately the inverse of a mock
 * window — its linear runs light-at-the-bottom (`0deg`) while windows run
 * light-at-the-top (`170deg`), so backdrop and window never read as one
 * surface.
 */
export const spotlightBackdrop = css({
  backgroundImage: [
    "radial-gradient(900px 620px at 50% 50%, #0e0e11 0%, rgba(10,10,12,0) 72%)",
    "radial-gradient(1200px 780px at 30% 20%, #1d1d24 0%, rgba(29,29,36,0) 64%)",
    "radial-gradient(820px 520px at 0% 0%, rgba(94,106,210,.14) 0%, rgba(94,106,210,0) 60%)",
    "linear-gradient(135deg, #1b1b21 0%, #070708 55%, #040405 100%)",
  ].join(", "),
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

/**
 * The shared soft illustration backdrop: container chrome plus a layered
 * gradient (accent wash over surface lifts). One static class per tone, used
 * by `IllustrationStage` and the auto-switch tab pane alike, so the wash
 * never drifts between copies.
 *
 * Static records rather than a function: Panda only emits `css()` calls it
 * can resolve at build time, and an interpolated value would silently drop
 * the whole gradient from the built CSS.
 */
const stageBackdropBase = {
  position: "relative",
  overflow: "hidden",
  borderWidth: "hairline",
  borderStyle: "solid",
  borderColor: "border.subtle",
} as const;

export const stageBackdrops = {
  indigo: css({
    ...stageBackdropBase,
    backgroundImage: [
      "radial-gradient(110% 90% at 0% 0%, token(colors.accent.subtle) 0%, transparent 55%)",
      "radial-gradient(130% 100% at 18% 8%, token(colors.surface.active) 0%, transparent 64%)",
      "radial-gradient(150% 110% at 50% 118%, token(colors.surface.subtle) 0%, transparent 60%)",
      "linear-gradient(135deg, token(colors.surface.hover) 0%, token(colors.canvas) 78%, token(colors.canvas) 100%)",
    ].join(", "),
  }),
  teal: css({
    ...stageBackdropBase,
    backgroundImage: [
      "radial-gradient(110% 90% at 0% 0%, token(colors.success.subtle) 0%, transparent 55%)",
      "radial-gradient(130% 100% at 18% 8%, token(colors.surface.active) 0%, transparent 64%)",
      "radial-gradient(150% 110% at 50% 118%, token(colors.surface.subtle) 0%, transparent 60%)",
      "linear-gradient(135deg, token(colors.surface.hover) 0%, token(colors.canvas) 78%, token(colors.canvas) 100%)",
    ].join(", "),
  }),
  warm: css({
    ...stageBackdropBase,
    backgroundImage: [
      "radial-gradient(110% 90% at 0% 0%, token(colors.warning.subtle) 0%, transparent 55%)",
      "radial-gradient(130% 100% at 18% 8%, token(colors.surface.active) 0%, transparent 64%)",
      "radial-gradient(150% 110% at 50% 118%, token(colors.surface.subtle) 0%, transparent 60%)",
      "linear-gradient(135deg, token(colors.surface.hover) 0%, token(colors.canvas) 78%, token(colors.canvas) 100%)",
    ].join(", "),
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
