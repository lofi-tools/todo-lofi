# Shepherd

Desktop shell for Shepherd: a Tauri 2 window over the SolidJS UI in `ui/`. Task
behaviour lives in `libs/shepherd-core`; this directory owns only the window, the
invoke commands and the event channel.

## Run

From the repository root, inside the dev shell (`direnv allow .` once; the shell
then loads it automatically):

```sh
pnpm install
pnpm dev:shepherd
```

`pnpm dev:shepherd` runs the Tauri CLI from this directory, which starts the Vite
dev server in `ui/` on port 1420 and then opens the desktop window. Non-interactive
runners need the wrapper: `direnv exec . pnpm dev:shepherd`.

Design and build order: [docs/todo.md](./docs/todo.md).
