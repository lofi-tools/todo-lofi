/**
 * Drives an auto-advancing tab set: a `role="tablist"` of `role="tab"` buttons
 * and their `role="tabpanel"` panels.
 *
 * Cycling runs by default and pauses only while the reader is on what they are
 * reading: the active tab, the panel area, or anywhere focus is inside the
 * component. Hovering the other tabs or the space around them does not stop the
 * clock. Pausing holds the time that is left, so resuming continues the cycle
 * instead of starting it over, which keeps the timer and the progress bar in
 * step. Picking a tab starts that tab's full dwell. Panels cross-fade on every
 * change, and `prefers-reduced-motion` turns both the cycling and the fade off.
 */

interface Options {
  /** How long each tab stays active, in milliseconds. */
  duration?: number;
  /** Called with the index whenever the active tab changes. */
  onSelect?: (index: number) => void;
  /** Called when cycling pauses or resumes. */
  onToggle?: (running: boolean) => void;
}

const FADE = 180;

export const startAutoSwitch = (root: HTMLElement, options: Options = {}) => {
  const { duration = 6000, onSelect, onToggle } = options;

  const tabs = Array.from(root.querySelectorAll<HTMLElement>('[role="tab"]'));
  const panels = Array.from(root.querySelectorAll<HTMLElement>('[role="tabpanel"]'));
  if (tabs.length === 0) return;

  // The pane is the panel area the reader pauses on; it is marked explicitly,
  // with the panels' own parent as the fallback for simpler tab sets.
  const pane =
    root.querySelector<HTMLElement>("[data-tab-pane]") ?? panels[0]?.parentElement ?? null;

  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  let active = 0;
  let timer: number | undefined;
  /** Clock time the current dwell ends at, while the timer is running. */
  let deadline = 0;
  /** Time left in the current dwell, held across a pause. */
  let remaining = duration;
  let focused = false;
  const hovered = new Set<HTMLElement>();
  let fades = 0;

  /** Cycling runs unless motion is reduced, focus is inside, or the reader is hovering a tab or the pane they are reading. */
  const runs = () =>
    !still &&
    !focused &&
    !Array.from(hovered).some((element) => element === tabs[active] || element === pane);

  /** Hide every panel but `index`, fading the outgoing one away first. */
  const showPanel = (index: number) => {
    const next = panels[index];
    if (!next) return;

    const current = panels.find((panel) => !panel.hidden);
    if (current === next) return;

    if (still || !current) {
      for (const panel of panels) panel.hidden = panel !== next;
      return;
    }

    const turn = ++fades;
    const animation = current.animate([{ opacity: 1 }, { opacity: 0 }], {
      duration: FADE,
      easing: "ease",
      fill: "forwards",
    });
    animation.finished.then(
      () => {
        // A newer switch took over while this fade ran; it owns the panels.
        if (turn !== fades) return;
        current.hidden = true;
        // Drop the forwards fill so the panel is clean the next time it shows.
        animation.cancel();
        next.hidden = false;
        next.animate([{ opacity: 0 }, { opacity: 1 }], { duration: FADE, easing: "ease" });
      },
      () => {},
    );
  };

  const select = (index: number) => {
    active = index;
    // The new tab gets its full dwell: the progress bar restarts, so the timer
    // has to as well or the bar would run out ahead of the switch.
    remaining = duration;
    if (timer !== undefined) {
      window.clearTimeout(timer);
      timer = undefined;
    }

    tabs.forEach((tab, position) => {
      const on = position === index;
      tab.setAttribute("aria-selected", String(on));
      tab.tabIndex = on ? 0 : -1;
      tab.toggleAttribute("data-active", on);
    });
    showPanel(index);
    onSelect?.(index);
    // The reader may already be hovering the tab that just became active.
    sync();
  };

  const advance = () => {
    timer = undefined;
    select((active + 1) % tabs.length);
  };

  const sync = () => {
    const running = runs();
    if (running && timer === undefined) {
      deadline = performance.now() + remaining;
      timer = window.setTimeout(advance, remaining);
    } else if (!running && timer !== undefined) {
      window.clearTimeout(timer);
      timer = undefined;
      // Hold what is left so resuming carries the cycle on, in step with the bar.
      remaining = Math.max(0, deadline - performance.now());
    }
    onToggle?.(running);
  };

  /** True while the reader is dragging a selection across this tab's text. */
  const selectingText = (element: HTMLElement) => {
    const selection = window.getSelection();
    return Boolean(selection && !selection.isCollapsed && element.contains(selection.anchorNode));
  };

  const watchHover = (element: HTMLElement) => {
    element.addEventListener("mouseenter", () => {
      hovered.add(element);
      sync();
    });
    element.addEventListener("mouseleave", () => {
      hovered.delete(element);
      sync();
    });
  };

  tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => {
      if (selectingText(tab)) return;
      select(index);
    });
    tab.addEventListener("keydown", (event) => {
      if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(event.key)) return;
      event.preventDefault();
      const forward = event.key === "ArrowDown" || event.key === "ArrowRight";
      const next = (index + (forward ? 1 : -1) + tabs.length) % tabs.length;
      select(next);
      tabs[next]?.focus();
    });
    watchHover(tab);
  });

  if (pane) watchHover(pane);

  root.addEventListener("focusin", () => {
    focused = true;
    sync();
  });
  root.addEventListener("focusout", (event) => {
    const next = (event as FocusEvent).relatedTarget as Node | null;
    if (next && root.contains(next)) return;
    focused = false;
    sync();
  });

  select(0);
};
