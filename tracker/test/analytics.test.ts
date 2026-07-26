import { describe, it, expect, beforeEach, vi } from 'vitest'
import { Analytics } from '../src/index'

const ENDPOINT = 'https://example.com/collect'
const SITE_ID = 'test-site'

async function getPayloadFromCall(
  call: unknown[],
): Promise<Record<string, unknown>> {
  const blob = call[1] as Blob
  return JSON.parse(await blob.text())
}

describe('Analytics', () => {
  beforeEach(() => {
    vi.restoreAllMocks()
    window.innerWidth = 1440
  })

  describe('constructor', () => {
    it('throws without endpoint', () => {
      // @ts-expect-error testing missing endpoint
      expect(() => new Analytics({})).toThrow('endpoint is required')
    })

    it('creates instance with endpoint', () => {
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })
      expect(a).toBeInstanceOf(Analytics)
      a.stop()
    })
  })

  describe('track()', () => {
    it('sends event payload', async () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })

      a.track('signup', { plan: 'pro' })

      expect(spy).toHaveBeenCalledTimes(1)
      const payload = await getPayloadFromCall(spy.mock.calls[0]!)
      expect(payload.t).toBe('event')
      expect(payload.n).toBe('signup')
      expect(payload.pr).toEqual({ plan: 'pro' })

      a.stop()
    })

    it('does not track after stop', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })

      a.stop()
      a.track('should-not-send')

      expect(spy).not.toHaveBeenCalled()
    })
  })

  describe('page()', () => {
    it('sends pageview payload', async () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })

      a.page('/about')

      expect(spy).toHaveBeenCalledTimes(1)
      const payload = await getPayloadFromCall(spy.mock.calls[0]!)
      expect(payload.t).toBe('pageview')
      expect(payload.p).toBe('/about')

      a.stop()
    })

    it('does not track after stop', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })

      a.stop()
      a.page('/about')

      expect(spy).not.toHaveBeenCalled()
    })
  })

  describe('autoTrack', () => {
    it('fires page view on init', async () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)

      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      expect(spy).toHaveBeenCalledTimes(1)

      const payload = await getPayloadFromCall(spy.mock.calls[0]!)
      expect(payload.t).toBe('pageview')

      a.stop()
    })

    it('fires page view on pushState', async () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

      const before = spy.mock.calls.length
      history.pushState({}, '', '/new-page')

      expect(spy).toHaveBeenCalledTimes(before + 1)
      a.stop()
    })

    it('does not fire page view on replaceState', async () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

      const before = spy.mock.calls.length
      history.replaceState({}, '', '/replaced')

      expect(spy).toHaveBeenCalledTimes(before)
      a.stop()
    })

    it('does not fire on init when autoTrack is false', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)

      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })
      expect(spy).not.toHaveBeenCalled()

      a.stop()
    })

    it('does not fire pushState after stop', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      a.stop()

      const before = spy.mock.calls.length
      history.pushState({}, '', '/after-stop')

      expect(spy).toHaveBeenCalledTimes(before)
    })
  })

  describe('pageview dedupe', () => {
    it('does not emit a pageview on hashchange (the hash is stripped from the path)', () => {
      vi.useFakeTimers()
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      spy.mockClear()

      // Past the 500ms same-path window, so this would re-count without the fix.
      vi.advanceTimersByTime(600)
      window.dispatchEvent(new Event('hashchange'))

      expect(spy).not.toHaveBeenCalled()
      a.stop()
      vi.useRealTimers()
    })

    it('page() then a pushState to the same path emits only one pageview', async () => {
      history.replaceState({}, '', '/start')
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      spy.mockClear()

      a.page('/dash')
      history.pushState({}, '', '/dash')

      const all = await Promise.all(spy.mock.calls.map((c) => getPayloadFromCall(c)))
      const dashViews = all.filter((p) => p.t === 'pageview' && p.p === '/dash')
      expect(dashViews).toHaveLength(1)
      a.stop()
    })
  })

  describe('pageview dedupe', () => {
    it('does not emit a pageview on hashchange (the hash is stripped from the path)', () => {
      vi.useFakeTimers()
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      spy.mockClear()

      // Past the 500ms same-path window, so this would re-count without the fix.
      vi.advanceTimersByTime(600)
      window.dispatchEvent(new Event('hashchange'))

      expect(spy).not.toHaveBeenCalled()
      a.stop()
      vi.useRealTimers()
    })

    it('page() then a pushState to the same path emits only one pageview', async () => {
      history.replaceState({}, '', '/start')
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      spy.mockClear()

      a.page('/dash')
      history.pushState({}, '', '/dash')

      const all = await Promise.all(spy.mock.calls.map((c) => getPayloadFromCall(c)))
      const dashViews = all.filter((p) => p.t === 'pageview' && p.p === '/dash')
      expect(dashViews).toHaveLength(1)
      a.stop()
    })
  })

  describe('respectDNT', () => {
    it('does not send when DNT is enabled and respectDNT is true', () => {
      Object.defineProperty(navigator, 'doNotTrack', {
        value: '1',
        configurable: true,
      })
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)

      const a = new Analytics({
        endpoint: ENDPOINT,
        siteId: SITE_ID,
        autoTrack: false,
        respectDNT: true,
      })

      a.track('test')
      a.page('/test')
      expect(spy).not.toHaveBeenCalled()
      a.stop()

      Object.defineProperty(navigator, 'doNotTrack', {
        value: null,
        configurable: true,
      })
    })
  })

  describe('duplicate-instance guard', () => {
    it('disables a second instance and warns', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})

      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })
      const b = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })

      expect(warn).toHaveBeenCalled()

      a.track('one')
      b.track('two')
      expect(spy).toHaveBeenCalledTimes(1)

      a.stop()
      b.stop()
    })

    it('frees the slot after stop so a fresh instance can run', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })
      a.stop()

      const b = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: false })
      b.track('after-rotate')
      expect(spy).toHaveBeenCalledTimes(1)
      b.stop()
    })
  })

  describe('prerender', () => {
    it('defers initial pageview until prerenderingchange fires', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      Object.defineProperty(document, 'prerendering', {
        value: true,
        configurable: true,
      })

      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })
      expect(spy).not.toHaveBeenCalled()

      Object.defineProperty(document, 'prerendering', {
        value: false,
        configurable: true,
      })
      document.dispatchEvent(new Event('prerenderingchange'))

      expect(spy).toHaveBeenCalledTimes(1)
      a.stop()
    })
  })

  describe('view id (vid)', () => {
    beforeEach(() => {
      Object.defineProperty(document, 'visibilityState', {
        value: 'visible',
        configurable: true,
      })
    })

    it('shares one vid across pageview, event, and pageleave within a view', async () => {
      vi.useFakeTimers()
      vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

      vi.advanceTimersByTime(2000)
      a.track('cta_click')
      window.dispatchEvent(new Event('pagehide'))

      const all = await Promise.all(spy.mock.calls.map((c) => getPayloadFromCall(c)))
      const pv = all.find((p) => p.t === 'pageview')!
      const ev = all.find((p) => p.t === 'event')!
      const leave = all.find((p) => p.t === 'pageleave')!
      expect(typeof pv.vid).toBe('string')
      expect(pv.vid).toBeTruthy()
      expect(ev.vid).toBe(pv.vid)
      expect(leave.vid).toBe(pv.vid)

      a.stop()
      vi.useRealTimers()
    })

    it('regenerates vid on SPA nav; the outgoing pageleave keeps the old vid', async () => {
      vi.useFakeTimers()
      vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
      history.replaceState({}, '', '/start')
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

      const firstPv = await getPayloadFromCall(spy.mock.calls[0]!)
      const vid1 = firstPv.vid as string

      vi.advanceTimersByTime(2500)
      history.pushState({}, '', '/next')

      const all = await Promise.all(spy.mock.calls.map((c) => getPayloadFromCall(c)))
      const leave = all.find((p) => p.t === 'pageleave' && p.p === '/start')!
      const nextPv = all.find((p) => p.t === 'pageview' && p.p === '/next')!
      expect(leave.vid).toBe(vid1)
      expect(nextPv.vid).toBeTruthy()
      expect(nextPv.vid).not.toBe(vid1)

      a.stop()
      vi.useRealTimers()
    })
  })

  describe('stop()', () => {
    it('cleans up and prevents further tracking', () => {
      const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
      const a = new Analytics({ endpoint: ENDPOINT, siteId: SITE_ID, autoTrack: true })

      const callsAtInit = spy.mock.calls.length
      a.stop()

      a.track('event')
      a.page('/page')
      history.pushState({}, '', '/after-stop')

      expect(spy).toHaveBeenCalledTimes(callsAtInit)
    })
  })
})
