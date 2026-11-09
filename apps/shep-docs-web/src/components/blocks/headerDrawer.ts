/**
 * Drives the site header's mobile drawer: one toggle button, one panel. The
 * panel animates open in CSS (its grid row grows from `0fr` to `1fr`); this
 * only owns the state, so the markup and the transition stay declarative.
 *
 * Markup contract, all inside the header:
 * - `[data-header-drawer]` on the root that holds the toggle and the panel.
 * - `[data-drawer-toggle]` on the button, carrying `aria-expanded` and
 *   `aria-controls`.
 * - `[data-drawer-panel]` on the panel, which opens on its `[data-open]`.
 */

/** The drawer only exists below `sm`; at or above it the nav shows the links. */
const WIDE = "(min-width: 640px)";

export const startHeaderDrawers = () => {
  for (const root of document.querySelectorAll<HTMLElement>("[data-header-drawer]")) {
    const toggle = root.querySelector<HTMLButtonElement>("[data-drawer-toggle]");
    const panel = root.querySelector<HTMLElement>("[data-drawer-panel]");
    if (!toggle || !panel) continue;

    const setOpen = (open: boolean) => {
      toggle.setAttribute("aria-expanded", String(open));
      panel.toggleAttribute("data-open", open);
      // A collapsed panel is still in the flow, so keep it out of reach.
      panel.toggleAttribute("inert", !open);
    };

    toggle.addEventListener("click", () => {
      setOpen(toggle.getAttribute("aria-expanded") !== "true");
    });

    // Picking a link closes the drawer; the page is navigating anyway.
    panel.addEventListener("click", (event) => {
      if ((event.target as Element | null)?.closest("a")) setOpen(false);
    });

    root.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      if (toggle.getAttribute("aria-expanded") !== "true") return;
      setOpen(false);
      toggle.focus();
    });

    // Widening past the breakpoint hides the drawer and its button; drop the
    // open state so coming back to a narrow window starts closed.
    window.matchMedia(WIDE).addEventListener("change", (event) => {
      if (event.matches) setOpen(false);
    });

    setOpen(false);
  }
};
