import { fileURLToPath } from 'node:url'
import { defineConfig } from 'astro/config'
import solid from '@astrojs/solid-js'

export default defineConfig({
  integrations: [solid()],
  // Keep the two docs sites separate: Taskstream uses 4321, Shepherd 4323.
  server: { port: 4323 },
  // Playwright previews the build on its own port.
  preview: { port: 4324 },
  vite: {
    resolve: {
      // Panda generates `styled-system` per project; the alias lets page code
      // import from it the way the design system's components do internally.
      alias: {
        'styled-system': fileURLToPath(new URL('./styled-system', import.meta.url)),
      },
    },
  },
})
