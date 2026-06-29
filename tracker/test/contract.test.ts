import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import pageviewFixture from './fixtures/wire/pageview.json'
import eventFixture from './fixtures/wire/event.json'
import performanceFixture from './fixtures/wire/performance.json'
import pageleaveFixture from './fixtures/wire/pageleave.json'
import {
  buildPageViewPayload,
  buildEventPayload,
  buildPerformancePayload,
  buildPageLeavePayload,
} from '../src/payload'

// The fixtures under fixtures/wire/ are the shared client<->server wire contract;
// server/tests/contract.rs deserializes the same files. If the client builders
// and the fixtures drift apart, this fails; if the server and the fixtures drift,
// the Rust test fails. The builders never add `s`/`vid` (the Analytics instance
// stamps those), so they're stripped before comparing.
const FIXED_TS = 1735732800000

function wireFields(o: Record<string, unknown>): Record<string, unknown> {
  const { s: _s, vid: _vid, ...rest } = o
  return rest
}

describe('wire contract (client builders match the shared fixtures)', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date(FIXED_TS))
  })
  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('pageview', () => {
    window.innerWidth = 1280
    Object.defineProperty(document, 'referrer', {
      value: 'https://google.com/x',
      configurable: true,
    })
    history.replaceState(
      {},
      '',
      '/about?utm_source=newsletter&utm_medium=email&utm_campaign=launch',
    )
    expect(buildPageViewPayload('/about')).toEqual(
      wireFields(pageviewFixture as Record<string, unknown>),
    )
  })

  it('event', () => {
    history.replaceState({}, '', '/pricing')
    expect(buildEventPayload('cta_click', { plan: 'pro' })).toEqual(
      wireFields(eventFixture as Record<string, unknown>),
    )
  })

  it('performance', () => {
    history.replaceState({}, '', '/')
    expect(
      buildPerformancePayload({ lcp: 1200, fcp: 800, cls: 0.05, inp: 90, ttfb: 150 }),
    ).toEqual(wireFields(performanceFixture as Record<string, unknown>))
  })

  it('pageleave', () => {
    expect(buildPageLeavePayload('/article', 45000)).toEqual(
      wireFields(pageleaveFixture as Record<string, unknown>),
    )
  })
})
