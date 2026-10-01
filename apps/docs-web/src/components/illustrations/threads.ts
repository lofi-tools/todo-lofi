/**
 * Draws the curved threads that link two surfaces in an illustration. Shared by
 * the "centered on you" and "two-way sync" mockups so their links stay
 * consistent.
 *
 * Markup contract:
 * - `[data-threads]` on the positioned root establishes the coordinate space.
 * - An `<svg data-thread-svg>` overlay fills that root.
 * - `g[data-thread][data-from][data-to]` pairs a source id with a target id and
 *   holds one `[data-thread-path]` plus at least two `[data-thread-node]`.
 * - Anchors carry `data-thread-from="<id>"` / `data-thread-to="<id>"`.
 */
const drawThreads = () => {
  for (const root of document.querySelectorAll<HTMLElement>("[data-threads]")) {
    const svg = root.querySelector<SVGSVGElement>("[data-thread-svg]");
    if (!svg) continue;

    const box = root.getBoundingClientRect();
    // A hidden tab panel has no box yet; the observer draws once it is shown.
    if (!box.width || !box.height) continue;
    svg.setAttribute("viewBox", `0 0 ${box.width} ${box.height}`);

    for (const group of root.querySelectorAll<SVGGElement>("g[data-thread]")) {
      const from = root.querySelector<HTMLElement>(`[data-thread-from="${group.dataset.from}"]`);
      const to = root.querySelector<HTMLElement>(`[data-thread-to="${group.dataset.to}"]`);
      const path = group.querySelector<SVGPathElement>("[data-thread-path]");
      const nodes = group.querySelectorAll<SVGCircleElement>("[data-thread-node]");
      if (!from || !to || !path || nodes.length < 2) continue;

      const start = from.getBoundingClientRect();
      const end = to.getBoundingClientRect();
      // Connect the facing edges, so the thread works whichever side of the app
      // the target sits on.
      const forward = end.left + end.width / 2 >= start.left + start.width / 2;
      const x1 = (forward ? start.right : start.left) - box.left;
      const y1 = start.top + start.height / 2 - box.top;
      const x2 = (forward ? end.left : end.right) - box.left;
      const y2 = end.top + end.height / 2 - box.top;

      // Pull both control points sideways so the connector leaves and lands
      // flat, then bows between the two ends: a loose thread, not a ruler.
      const reach = Math.min(56, Math.max(10, Math.abs(x2 - x1) * 0.5)) * (forward ? 1 : -1);
      path.setAttribute(
        "d",
        `M ${x1} ${y1} C ${x1 + reach} ${y1}, ${x2 - reach} ${y2}, ${x2} ${y2}`,
      );
      nodes[0]?.setAttribute("cx", String(x1));
      nodes[0]?.setAttribute("cy", String(y1));
      nodes[1]?.setAttribute("cx", String(x2));
      nodes[1]?.setAttribute("cy", String(y2));
    }
  }
};

/** Draws every thread on the page and redraws them as the layout changes. */
export const observeThreads = () => {
  drawThreads();
  window.addEventListener("resize", drawThreads);

  if (typeof ResizeObserver === "undefined") return;
  const observer = new ResizeObserver(drawThreads);
  for (const root of document.querySelectorAll<HTMLElement>("[data-threads]")) {
    observer.observe(root);
  }
};
