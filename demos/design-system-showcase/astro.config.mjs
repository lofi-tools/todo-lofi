import { fileURLToPath } from 'node:url'
import { defineConfig } from 'astro/config'
import solid from '@astrojs/solid-js'

export default defineConfig({
  integrations: [solid()],
  vite: {
    resolve: {
      // Panda generates `styled-system` per project; the alias lets showcase
      // code import from it the way the design system does internally.
      alias: {
        'styled-system': fileURLToPath(new URL('./styled-system', import.meta.url)),
      },
    },
  },
})
