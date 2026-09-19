# Token extraction record

Per decision #17/#19, the palette, type scale, spacing, radii, shadows, and
motion values in this directory started as a **one-off extraction** from
linear.app. The extraction script was not kept; only the resulting token files
are committed, as decided.

## Method

1. Open each URL below in a headless Chromium at a **1440 × 900** viewport.
2. Read `getComputedStyle` for the page's structural elements (body, headings,
   nav, buttons, cards, borders) and enumerate CSS custom properties from
   `document.documentElement`'s computed style.
3. Dump raw values to a scratch JSON file.
4. Normalize by hand into the scales in this directory: Linear's values are
   minified and not a designed scale, so raw values were rounded, de-duplicated,
   and ordered. Only **values** were taken — no class names, selectors, or CSS
   text were copied.

## URLs read

- https://linear.app/homepage
- https://linear.app/pricing
- https://linear.app/features
- https://linear.app/customers
- https://linear.app/changelog
- https://linear.app/docs
- https://linear.app/method
- https://linear.app/security

## Date and viewport

- Extract date: 2026-09-19
- Viewport: 1440 × 900, device scale factor 1, dark color scheme

## Known deviations

- Inter Variable replaces whatever Linear ships; it is the closest open
  substitute and is self-hosted (decision #20).
- Mock product data, logos, names, and copy are invented placeholders
  (decision #9). No Linear wordmark or screenshot is reproduced.
- Token values are hand-normalized, so they are "indistinguishable at a
  glance", not byte-identical (spec §4.4).
