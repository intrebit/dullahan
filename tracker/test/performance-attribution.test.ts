import { describe, it, expect, vi, beforeEach } from 'vitest'

// Capture the report callback the real module would flush asynchronously, so the
// test can fire it at a chosen moment (after an SPA navigation).
const hoisted = vi.hoisted(() => ({
  report: null as ((m: Record<string, number>) => void) | null,
}))

vi.mock('../src/performance', () => ({
  startPerformanceTracking: (report: (m: Record<string, number>) => void) => {
    hoisted.report = report
    return () => {}
  },
}))

import { Analytics } from '../src/index'

const ENDPOINT = 'https://example.com/collect'
const SITE_ID = 'test-site'

async function payload(call: unknown[]): Promise<Record<string, unknown>> {
  return JSON.parse(await (call[1] as Blob).text())
}

describe('performance metric attribution', () => {
  beforeEach(() => {
    vi.restoreAllMocks()
    hoisted.report = null
    Object.defineProperty(document, 'visibilityState', {
      value: 'visible',
      configurable: true,
    })
  })

  it('pins metrics to the view active when tracking started, even after an SPA nav', async () => {
    history.replaceState({}, '', '/measured')
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    const firstPv = await payload(spy.mock.calls[0]!)
    const vid1 = firstPv.vid as string
    expect(vid1).toBeTruthy()

    // SPA navigation regenerates the live view id BEFORE the perf flush lands.
    history.pushState({}, '', '/next')

    // Web vitals flush now (deferred in the real module).
    hoisted.report!({ lcp: 1200 })

    const all = await Promise.all(spy.mock.calls.map(payload))
    const perf = all.find((p) => p.t === 'performance')!
    expect(perf).toBeTruthy()
    expect(perf.vid).toBe(vid1)
    expect(perf.p).toBe('/measured')

    a.stop()
  })
})
