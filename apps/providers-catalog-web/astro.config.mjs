import { fileURLToPath } from 'node:url'
import { defineConfig } from 'astro/config'
import solid from '@astrojs/solid-js'

export default defineConfig({
  integrations: [solid()],
  // The catalog is a third site: docs use 4321/4323, previews use 4322/4324.
  server: { port: 4325 },
  preview: { port: 4326 },
  vite: {
    resolve: {
      alias: {
        'styled-system': fileURLToPath(new URL('./styled-system', import.meta.url)),
      },
    },
  },
})
