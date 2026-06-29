import { describe, it, expect, vi, beforeEach } from 'vitest'

async function payloadOf(call: unknown[]): Promise<Record<string, unknown>> {
  return JSON.parse(await (call[1] as Blob).text())
}

function injectScript(attrs: Record<string, string>): void {
  const s = document.createElement('script')
  for (const [k, v] of Object.entries(attrs)) s.setAttribute(k, v)
  document.head.appendChild(s)
}

describe('auto (script-tag bootstrap)', () => {
  beforeEach(() => {
    vi.resetModules()
    vi.restoreAllMocks()
    delete (globalThis as Record<string, unknown>).__dullahan_active__
    delete (window as unknown as Record<string, unknown>).dullahan
    Object.defineProperty(document, 'currentScript', { value: null, configurable: true })
    document.head.innerHTML = ''
    document.body.innerHTML = ''
  })

  it('starts tracking and fires a pageview from data-site', async () => {
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    injectScript({ 'data-site': 'demo', 'data-endpoint': 'https://x.test/collect' })

    await import('../src/auto')

    expect(spy).toHaveBeenCalledTimes(1)
    const body = await payloadOf(spy.mock.calls[0]!)
    expect(body.t).toBe('pageview')
    expect(body.s).toBe('demo')
  })

  it('exposes window.dullahan.track for inline custom events', async () => {
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    injectScript({ 'data-site': 'demo', 'data-endpoint': 'https://x.test/collect' })

    await import('../src/auto')
    spy.mockClear()

    const pt = (window as unknown as { dullahan: { track: (n: string, p?: unknown) => void } })
      .dullahan
    pt.track('signup', { plan: 'pro' })

    expect(spy).toHaveBeenCalledTimes(1)
    const body = await payloadOf(spy.mock.calls[0]!)
    expect(body.t).toBe('event')
    expect(body.n).toBe('signup')
  })

  it('defaults the endpoint to the script origin + /collect', async () => {
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)
    // Use document.currentScript (not appended to the DOM) so happy-dom doesn't
    // try to fetch the external src; this is also the real-world path for a
    // synchronously-executed <script>.
    const el = document.createElement('script')
    el.setAttribute('data-site', 'demo')
    Object.defineProperty(el, 'src', {
      value: 'https://cdn.test/pt.js',
      configurable: true,
    })
    Object.defineProperty(document, 'currentScript', { value: el, configurable: true })

    await import('../src/auto')

    expect(spy).toHaveBeenCalledTimes(1)
    expect(spy.mock.calls[0]![0]).toBe('https://cdn.test/collect')
  })

  it('warns and starts nothing when data-site is absent', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const spy = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true)

    await import('../src/auto')

    expect(warn).toHaveBeenCalled()
    expect(spy).not.toHaveBeenCalled()
  })
})
