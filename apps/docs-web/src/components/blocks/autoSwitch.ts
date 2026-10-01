/**
 * Drives an auto-advancing tab set: a `role="tablist"` of `role="tab"` buttons
 * and their `role="tabpanel"` panels.
 *
 * Cycling pauses whenever the pointer or focus is anywhere inside the
 * component, and picking a tab leaves it put until the reader moves on —
 * clicking no longer restarts the clock. Panels cross-fade on every change,
 * and `prefers-reduced-motion` turns both the cycling and the fade off.
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

  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  let active = 0;
  let timer: number | undefined;
  let paused = false;
  let fades = 0;

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
    tabs.forEach((tab, position) => {
      const on = position === index;
      tab.setAttribute("aria-selected", String(on));
      tab.tabIndex = on ? 0 : -1;
      tab.toggleAttribute("data-active", on);
    });
    showPanel(index);
    onSelect?.(index);
  };

  const sync = () => {
    const running = !paused && !still;
    if (running && timer === undefined) {
      timer = window.setInterval(() => select((active + 1) % tabs.length), duration);
    } else if (!running && timer !== undefined) {
      window.clearInterval(timer);
      timer = undefined;
    }
    onToggle?.(running);
  };

  const pause = () => {
    paused = true;
    sync();
  };

  const resume = () => {
    paused = false;
    sync();
  };

  /** True while the reader is dragging a selection across this tab's text. */
  const selectingText = (element: HTMLElement) => {
    const selection = window.getSelection();
    return Boolean(selection && !selection.isCollapsed && element.contains(selection.anchorNode));
  };

  tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => {
      if (selectingText(tab)) return;
      select(index);
      // The reader chose this tab; it waits for them to leave the component.
      pause();
    });
    tab.addEventListener("keydown", (event) => {
      if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(event.key)) return;
      event.preventDefault();
      const forward = event.key === "ArrowDown" || event.key === "ArrowRight";
      const next = (index + (forward ? 1 : -1) + tabs.length) % tabs.length;
      select(next);
      tabs[next]?.focus();
    });
  });

  root.addEventListener("mouseenter", pause);
  root.addEventListener("mouseleave", resume);
  root.addEventListener("focusin", pause);
  root.addEventListener("focusout", resume);

  select(0);
  sync();
};
