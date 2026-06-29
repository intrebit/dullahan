import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { startClickTracking, type ClickTracker } from '../src/clicks'

interface Hit {
  name: string
  props: Record<string, unknown>
}

function anchor(href: string, opts: { download?: boolean; child?: boolean } = {}): Element {
  const a = document.createElement('a')
  a.setAttribute('href', href)
  if (opts.download) a.setAttribute('download', '')
  document.body.appendChild(a)
  if (opts.child) {
    const span = document.createElement('span')
    a.appendChild(span)
    return span
  }
  return a
}

describe('startClickTracking', () => {
  let tracker: ClickTracker | null = null
  let hits: Hit[]

  beforeEach(() => {
    hits = []
    document.body.innerHTML = ''
    tracker = startClickTracking((name, props) => hits.push({ name, props }))
  })

  afterEach(() => {
    tracker?.stop()
    tracker = null
  })

  it('emits outbound for a link to another host', () => {
    anchor('https://external.example.com/page?ref=x').dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    expect(hits).toEqual([
      { name: 'outbound', props: { href: 'https://external.example.com/page' } },
    ])
  })

  it('ignores same-origin navigations', () => {
    anchor('/internal/path').dispatchEvent(new MouseEvent('click', { bubbles: true }))
    expect(hits).toEqual([])
  })

  it('emits download for a known file extension', () => {
    anchor('https://files.example.com/report.pdf').dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    expect(hits).toEqual([
      { name: 'download', props: { href: 'https://files.example.com/report.pdf' } },
    ])
  })

  it('emits download for a same-origin link with the download attribute', () => {
    anchor('/files/export', { download: true }).dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    expect(hits).toHaveLength(1)
    expect(hits[0]!.name).toBe('download')
  })

  it('resolves the anchor from a nested click target', () => {
    anchor('https://external.example.com/x', { child: true }).dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    expect(hits).toHaveLength(1)
    expect(hits[0]!.name).toBe('outbound')
  })

  it('ignores non-http(s) protocols like mailto:', () => {
    anchor('mailto:hi@example.com').dispatchEvent(new MouseEvent('click', { bubbles: true }))
    expect(hits).toEqual([])
  })

  it('emits nothing after stop()', () => {
    tracker!.stop()
    anchor('https://external.example.com/x').dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    )
    expect(hits).toEqual([])
  })
})
