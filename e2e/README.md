# dullahan e2e

End-to-end test of the **real** browser → server-hosted `/pt.js` → `/collect` →
`/stats` path. A headless Chromium loads a page whose only integration is the
one-line `<script src="/pt.js" data-site=…>` snippet; the test then asserts the
pageview and a `window.dullahan.track(...)` custom event land in `/stats`.

It complements the fast unit/contract layers (which mock the network) by
exercising the actual served bundle, real `sendBeacon`, and real HTTP ingest.

## Run locally

Needs a `dullahan_e2e` Postgres database (the server creates the tables on boot):

```bash
createdb dullahan_e2e        # once
npm ci
npx playwright install chromium
npm test
```

Playwright builds the client (so `/pt.js` is the real bundle) and runs the server
via [`run-server.sh`](run-server.sh) on `127.0.0.1:3099` (override with
`DATABASE_URL` / `E2E_PORT`). With a server already running on that port it is
reused.

## Notes

- The fixture is fulfilled at the **server's own origin** so `/pt.js` and
  `/collect` are same-origin — the common same-domain deploy, and it avoids the
  test browser's private-network-access blocking of cross-origin loopback
  requests. (Production cross-origin, e.g. `analytics.example.com`, is
  public-to-public and unaffected; CORS for `/collect` is covered by the server
  test suite.)
- In CI this job is independent; consider keeping it non-required until it has a
  few green runs, since browser E2Es are the flakiest layer.
