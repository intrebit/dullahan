import { describe, it, expect, vi, afterEach } from 'vitest'
import { sendPayload } from '../src/transport'
import type { Payload } from '../src/types'

const ENDPOINT = 'https://example.com/collect'
const PAYLOAD: Payload = { t: 'event', s: 'site', p: '/', ts: 1, n: 'x' }

describe('sendPayload', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('uses sendBeacon and does not fall back when it succeeds', () => {
    const beacon = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    const fetchSpy = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('fetch', fetchSpy)

    sendPayload(PAYLOAD, ENDPOINT)

    expect(beacon).toHaveBeenCalledTimes(1)
    expect(fetchSpy).not.toHaveBeenCalled()
  })

  it('falls back to fetch when sendBeacon returns false', () => {
    vi.spyOn(navigator, 'sendBeacon').mockReturnValue(false)
    const fetchSpy = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('fetch', fetchSpy)

    sendPayload(PAYLOAD, ENDPOINT)

    expect(fetchSpy).toHaveBeenCalledTimes(1)
  })

  it('falls back to fetch when sendBeacon throws (e.g. body over the queue limit)', () => {
    vi.spyOn(navigator, 'sendBeacon').mockImplementation(() => {
      throw new DOMException('exceeds quota', 'QuotaExceededError')
    })
    const fetchSpy = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('fetch', fetchSpy)

    sendPayload(PAYLOAD, ENDPOINT)

    expect(fetchSpy).toHaveBeenCalledTimes(1)
  })
})
