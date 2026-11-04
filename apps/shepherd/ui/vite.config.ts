import { defineConfig } from 'vite'
import solid from 'vite-plugin-solid'

// The dev server port is fixed because `tauri.conf.json` loads exactly this
// origin; a fallback port would leave the window pointing at nothing.
export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
})
