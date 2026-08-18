/// <reference types="vitest/config" />
import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'

// Dev-only proxy: forwards /api/* to a running docket-core instance so the
// browser never has to make a cross-origin request (docket-core does not
// send CORS headers).
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const coreUrl = env.VITE_DOCKET_CORE_URL || 'http://127.0.0.1:8420'

  return {
    plugins: [react()],
    server: {
      proxy: {
        '/api': {
          target: coreUrl,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/api/, ''),
        },
      },
    },
    // jsdom (not the default 'node' environment) so component tests that
    // touch the DOM — e.g. rendering sanitized HTML — have a `document`.
    test: {
      environment: 'jsdom',
    },
  }
})
