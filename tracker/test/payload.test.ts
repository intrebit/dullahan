import { describe, it, expect } from 'vitest'
import {
  buildPageViewPayload,
  buildEventPayload,
  buildPerformancePayload,
} from '../src/payload'

describe('buildPageViewPayload', () => {
  it('returns pageview payload with correct type', () => {
    const payload = buildPageViewPayload()
    expect(payload.t).toBe('pageview')
    expect(typeof payload.p).toBe('string')
    expect(typeof payload.ts).toBe('number')
    expect(typeof payload.v).toBe('number')
  })

  it('accepts custom path', () => {
    const payload = buildPageViewPayload('/custom')
    expect(payload.p).toBe('/custom')
  })

  it('strips query params from custom path', () => {
    const payload = buildPageViewPayload('/custom?secret=token')
    expect(payload.p).toBe('/custom')
  })

  it('rounds viewport width', () => {
    Object.defineProperty(window, 'innerWidth', {
      value: 1443,
      configurable: true,
    })
    const payload = buildPageViewPayload()
    expect(payload.v).toBe(1440)
  })

  it('attaches utm campaign from the landing query string', () => {
    history.replaceState({}, '', '/landing?utm_source=news&utm_campaign=spring')
    const payload = buildPageViewPayload()
    expect(payload.u).toEqual({ s: 'news', c: 'spring' })
    expect(payload.p).toBe('/landing')
  })

  it('omits u when no utm params present', () => {
    history.replaceState({}, '', '/plain')
    const payload = buildPageViewPayload()
    expect(payload.u).toBeUndefined()
  })
})

describe('buildEventPayload', () => {
  it('returns event payload with name', () => {
    const payload = buildEventPayload('click')
    expect(payload.t).toBe('event')
    expect(payload.n).toBe('click')
    expect(payload.pr).toBeUndefined()
  })

  it('includes props when provided', () => {
    const payload = buildEventPayload('signup', { plan: 'pro' })
    expect(payload.pr).toEqual({ plan: 'pro' })
  })

  it('omits props when empty object', () => {
    const payload = buildEventPayload('click', {})
    expect(payload.pr).toBeUndefined()
  })

  it('includes path and timestamp', () => {
    const before = Date.now()
    const payload = buildEventPayload('test')
    const after = Date.now()
    expect(payload.ts).toBeGreaterThanOrEqual(before)
    expect(payload.ts).toBeLessThanOrEqual(after)
    expect(typeof payload.p).toBe('string')
  })
})

describe('buildPerformancePayload', () => {
  it('returns performance payload with metrics', () => {
    const payload = buildPerformancePayload({
      lcp: 1200,
      fcp: 800,
      cls: 0.05,
      ttfb: 300,
    })
    expect(payload.t).toBe('performance')
    expect(payload.pf).toEqual({
      lcp: 1200,
      fcp: 800,
      cls: 0.05,
      ttfb: 300,
    })
  })

  it('handles partial metrics', () => {
    const payload = buildPerformancePayload({ ttfb: 250 })
    expect(payload.pf).toEqual({ ttfb: 250 })
  })
})
