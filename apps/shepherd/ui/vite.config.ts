import { readFile } from 'node:fs/promises'
import { defineConfig, type Plugin } from 'vite'
import solid from 'vite-plugin-solid'

const smokeProject = process.env.SHEPHERD_DESKTOP_SMOKE_PROJECT

function desktopSmoke(project: string): Plugin {
  return {
    name: 'shepherd-terminal-smoke',
    apply: 'serve',
    config: () => ({
      define: {
        __SHEPHERD_SMOKE_FISH__: process.env.SHEPHERD_DESKTOP_SMOKE_FISH === '1',
        __SHEPHERD_SMOKE_PROJECT__: JSON.stringify(project),
        __SHEPHERD_SMOKE_PHASE__: JSON.stringify(process.env.SHEPHERD_DESKTOP_SMOKE_PHASE ?? 'prepare'),
      },
    }),
    transformIndexHtml: () => [
      { tag: 'script', attrs: { type: 'module', src: '/tests/desktop-terminal-smoke.ts' }, injectTo: 'body' },
    ],
    configureServer(server) {
      const openerLog = process.env.SHEPHERD_SMOKE_OPEN_LOG
      if (openerLog) {
        server.middlewares.use('/__shepherd-terminal-smoke/opener', (request, response) => {
          if (request.method !== 'GET') {
            response.writeHead(405).end()
            return
          }
          response.setHeader('Cache-Control', 'no-store')
          void readFile(openerLog, 'utf8').then(
            (url) => response.writeHead(200, { 'Content-Type': 'text/plain' }).end(url),
            (error: NodeJS.ErrnoException) => response.writeHead(error.code === 'ENOENT' ? 404 : 500).end(),
          )
        })
      }
      server.middlewares.use('/__shepherd-terminal-smoke', (request, response) => {
        if (request.method !== 'POST') {
          response.writeHead(405).end()
          return
        }
        let body = ''
        request.setEncoding('utf8')
        request.on('data', (chunk: string) => {
          body += chunk
          if (body.length > 4096) request.destroy()
        })
        request.on('end', () => {
          try {
            const result = JSON.parse(body)
            console.log('SHEPHERD_TERMINAL_SMOKE', JSON.stringify(result))
            response.writeHead(204).end()
          } catch {
            response.writeHead(400).end()
          }
        })
      })
    },
  }
}

// The dev server port is fixed because `tauri.conf.json` loads exactly this
// origin; a fallback port would leave the window pointing at nothing.
export default defineConfig({
  plugins: [solid(), ...(smokeProject ? [desktopSmoke(smokeProject)] : [])],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    ...(smokeProject ? { hmr: false } : {}),
  },
})
