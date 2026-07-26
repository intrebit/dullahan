import { defineConfig } from '@playwright/test'

const PORT = process.env.E2E_PORT ?? '3099'
const BASE = `http://127.0.0.1:${PORT}`

export default defineConfig({
  testDir: './tests',
  fullyParallel: false,
  retries: 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: BASE,
  },
  // Build the client (so /pt.js is the real bundle) and run the server.
  webServer: {
    command: './run-server.sh',
    url: `${BASE}/health`,
    timeout: 240_000,
    reuseExistingServer: !process.env.CI,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      // Fall back to the OS user, which is the usual local Postgres superuser
      // on a Homebrew/pgdg install. CI sets DATABASE_URL explicitly.
      DATABASE_URL:
        process.env.DATABASE_URL ??
        `postgres://${process.env.USER ?? 'postgres'}@localhost/dullahan_e2e`,
      ADMIN_TOKEN: 'e2e-token',
      BIND_ADDR: `127.0.0.1:${PORT}`,
    },
  },
})
