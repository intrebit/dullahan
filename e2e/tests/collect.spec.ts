import { test, expect, request, type APIRequestContext } from '@playwright/test'

const BASE = 'http://127.0.0.1:' + (process.env.E2E_PORT ?? '3099')
const ADMIN = 'e2e-token'
const SITE = 'e2e'

// Fulfilled at the server's OWN origin, so the relative /pt.js script and the
// derived /collect beacon are same-origin — the common same-domain deploy, and
// it sidesteps cross-origin private-network-access blocking in the test browser.
// The route only intercepts this navigation; /pt.js and /collect hit the real
// server. data-endpoint is omitted so the client must derive it from the script.
const FIXTURE_URL = `${BASE}/__e2e_fixture`
const PAGE_HTML = `<!doctype html><html><head>
  <script src="/pt.js" data-site="${SITE}"></script>
</head><body><h1>e2e</h1></body></html>`

async function summary(api: APIRequestContext): Promise<{ pageviews: number; events: number }> {
  const res = await api.get(`${BASE}/stats/summary?site=${SITE}&days=1`, {
    headers: { Authorization: `Bearer ${ADMIN}` },
  })
  expect(res.ok()).toBeTruthy()
  return res.json()
}

test('server-hosted /pt.js tracks pageviews and custom events end to end', async ({ page }) => {
  await page.route(FIXTURE_URL, (route) =>
    route.fulfill({ contentType: 'text/html', body: PAGE_HTML }),
  )
  await page.goto(FIXTURE_URL)

  // The served script self-initialized and exposed the global.
  await page.waitForFunction(() => 'dullahan' in window)

  // Fire a custom event the way an inline page script would.
  await page.evaluate(() => {
    ;(
      window as unknown as { dullahan: { track: (n: string, p?: unknown) => void } }
    ).dullahan.track('signup', { plan: 'pro' })
  })

  const api = await request.newContext()
  // Ingest is fire-and-forget (202 before the write), so poll.
  await expect
    .poll(async () => (await summary(api)).pageviews, { timeout: 15_000 })
    .toBeGreaterThanOrEqual(1)
  await expect
    .poll(async () => (await summary(api)).events, { timeout: 15_000 })
    .toBeGreaterThanOrEqual(1)
  await api.dispose()
})
