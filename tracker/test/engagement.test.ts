import { describe, it, expect, beforeEach, vi } from 'vitest'
import { Analytics } from '../src/index'

const ENDPOINT = 'https://example.com/collect'
const SITE_ID = 'test-site'

async function payload(call: unknown[]): Promise<Record<string, unknown>> {
  const blob = call[1] as Blob
  return JSON.parse(await blob.text())
}

function setVisibility(state: 'visible' | 'hidden') {
  Object.defineProperty(document, 'visibilityState', {
    value: state,
    configurable: true,
  })
  document.dispatchEvent(new Event('visibilitychange'))
}

function firePageHide() {
  window.dispatchEvent(new Event('pagehide'))
}

describe('engagement (pageleave)', () => {
  beforeEach(() => {
    vi.restoreAllMocks()
    vi.useRealTimers()
    Object.defineProperty(document, 'visibilityState', {
      value: 'visible',
      configurable: true,
    })
  })

  it('sends pageleave on pagehide with the visible time', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    vi.advanceTimersByTime(5_000)
    firePageHide()

    const all = await Promise.all(spy.mock.calls.map((c) => payload(c)))
    const leave = all.find((p) => p.t === 'pageleave')
    expect(leave).toBeDefined()
    expect(leave!.dur).toBe(5000)

    a.stop()
  })

  it('does not send on visibility=hidden alone (user may come back)', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    vi.advanceTimersByTime(5_000)
    setVisibility('hidden')

    const all = await Promise.all(spy.mock.calls.map((c) => payload(c)))
    expect(all.find((p) => p.t === 'pageleave')).toBeUndefined()

    a.stop()
  })

  it('accumulates across hide/show cycles and sums on pagehide', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    vi.advanceTimersByTime(3_000)
    setVisibility('hidden')
    vi.advanceTimersByTime(10_000) // hidden — should NOT count
    setVisibility('visible')
    vi.advanceTimersByTime(2_000)
    firePageHide()

    const all = await Promise.all(spy.mock.calls.map((c) => payload(c)))
    const leaves = all.filter((p) => p.t === 'pageleave')
    expect(leaves).toHaveLength(1)
    expect(leaves[0]!.dur).toBe(5000)

    a.stop()
  })

  it('skips pageleave when total visible time is under the minimum', async () => {
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    firePageHide()

    const all = await Promise.all(spy.mock.calls.map((c) => payload(c)))
    expect(all.find((p) => p.t === 'pageleave')).toBeUndefined()

    a.stop()
  })

  it('flushes outgoing path before next pageview on SPA route change', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    history.replaceState({}, '', '/start')
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    vi.advanceTimersByTime(2_500)
    history.pushState({}, '', '/next')

    const all = await Promise.all(spy.mock.calls.map((c) => payload(c)))
    const idxLeave = all.findIndex((p) => p.t === 'pageleave')
    const idxNext = all.findIndex((p) => p.t === 'pageview' && p.p === '/next')
    expect(idxLeave).toBeGreaterThanOrEqual(0)
    expect(idxNext).toBeGreaterThan(idxLeave)
    expect(all[idxLeave]!.p).toBe('/start')
    expect(all[idxLeave]!.dur).toBe(2500)

    a.stop()
  })

  it('caps dur at 30 minutes', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

    vi.advanceTimersByTime(2 * 60 * 60 * 1000)
    firePageHide()

    const all = await Promise.all(spy.mock.calls.map((c) => payload(c)))
    const leave = all.find((p) => p.t === 'pageleave')
    expect(leave!.dur).toBe(30 * 60 * 1000)

    a.stop()
  })
})
