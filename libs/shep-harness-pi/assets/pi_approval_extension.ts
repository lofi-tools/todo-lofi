// Loaded with --extension by the Pi harness. Every tool call waits for the
// approval card Shepherd renders; a denied call never executes.
export default function (pi) {
  pi.on("tool_call", async (event, ctx) => {
    const name = event.toolName || "tool";
    let allowed = false;
    try {
      allowed = await ctx.ui.confirm(
        `Allow Pi to run ${name}?`,
        JSON.stringify(event.input ?? {}, null, 2),
      );
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      return { block: true, reason: `Shepherd could not ask for approval: ${reason}` };
    }
    if (!allowed) return { block: true, reason: "Denied in Shepherd" };
  });
}
