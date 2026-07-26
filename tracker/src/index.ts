import type { AnalyticsConfig, Payload, PayloadInput } from './types'
import { checkDNT } from './privacy'
import { sendPayload } from './transport'
import {
  buildPageViewPayload,
  buildEventPayload,
  buildPageLeavePayload,
  getPath,
} from './payload'
import { startAutoTracking } from './collect'
import { startEngagement, type Engagement } from './engagement'

export type {
  AnalyticsConfig,
  Payload,
  PayloadInput,
  PageviewPayload,
  EventPayload,
  PageleavePayload,
} from './types'

const INSTANCE_KEY = '__dullahan_active__'

// Ephemeral per-view id: regenerated on every page view, kept only in memory,
// never persisted. Groups the events of a single pageload without being a
// durable visitor identity.
function newViewId(): string {
  const c = globalThis.crypto
  if (c && typeof c.randomUUID === 'function') return c.randomUUID()
  return Math.random().toString(36).slice(2, 10) + Math.random().toString(36).slice(2, 10)
}

export class Analytics {
  private config: Required<AnalyticsConfig>
  private cleanups: (() => void)[] = []
  private stopped = false
  private engagement: Engagement | null = null
  private currentViewId = ''
  private lastViewPath = ''
  private lastViewTime = 0

  constructor(config: AnalyticsConfig) {
    if (!config.endpoint) {
      throw new Error('Analytics: endpoint is required')
    }
    if (!config.siteId) {
      throw new Error('Analytics: siteId is required')
    }

    this.config = {
      endpoint: config.endpoint,
      siteId: config.siteId,
      autoTrack: config.autoTrack ?? true,
      respectDNT: config.respectDNT ?? false,
    }

    if (this.config.respectDNT && checkDNT()) {
      this.stopped = true
      return
    }

    // Guard against duplicate instances on the same page (snippet pasted twice,
    // SPA bundle re-evaluated on hot reload, etc). Doubling counts is a common
    // and hard-to-debug source of inflated metrics.
    const w = globalThis as Record<string, unknown>
    if (w[INSTANCE_KEY]) {
      if (typeof console !== 'undefined') {
        console.warn(
          'dullahan: an Analytics instance is already running on this page; new instance disabled',
        )
      }
      this.stopped = true
      return
    }
    w[INSTANCE_KEY] = true
    this.cleanups.push(() => {
      delete w[INSTANCE_KEY]
    })

    if (this.config.autoTrack) {
      this._startAutoTracking()
    }
  }

  private _send(payload: PayloadInput): void {
    if (this.stopped) return
    const full = { ...payload, s: this.config.siteId } as Payload
    if (full.vid === undefined && this.currentViewId) full.vid = this.currentViewId
    sendPayload(full, this.config.endpoint)
  }

  private _startAutoTracking(): void {
    const eng = startEngagement((path, dur) => {
      this._send(buildPageLeavePayload(path, dur))
    })
    this.engagement = eng
    this.cleanups.push(() => eng.stop())

    const fireView = (path?: string) => {
      const next = path ?? getPath()
      const now = Date.now()
      if (next === this.lastViewPath && now - this.lastViewTime < 500) return
      this.lastViewPath = next
      this.lastViewTime = now
      eng.flush() // emits the outgoing page's pageleave under the old view id
      this.currentViewId = newViewId()
      this._send(buildPageViewPayload(next))
      eng.reset(next)
    }
    this.cleanups.push(startAutoTracking(() => fireView()))

    const onPageShow = (e: PageTransitionEvent) => {
      if (e.persisted) fireView()
    }
    window.addEventListener('pageshow', onPageShow)
    this.cleanups.push(() => window.removeEventListener('pageshow', onPageShow))

    // Speculation-rules / Chromium prerender loads the page invisibly. Firing
    // a pageview during prerender double-counts whenever the user never lands
    // on the prerendered URL. Defer the initial view until activation.
    const prerendering =
      (document as Document & { prerendering?: boolean }).prerendering === true
    if (prerendering) {
      const onActivate = () => {
        document.removeEventListener('prerenderingchange', onActivate)
        fireView()
      }
      document.addEventListener('prerenderingchange', onActivate)
      this.cleanups.push(() =>
        document.removeEventListener('prerenderingchange', onActivate),
      )
    } else {
      fireView()
    }
  }

  /** Track a custom event. */
  track(name: string, props?: Record<string, unknown>): void {
    if (this.stopped) return
    this._send(buildEventPayload(name, props))
  }

  /** Manually track a page view. */
  page(path?: string): void {
    if (this.stopped) return
    this.engagement?.flush()
    const next = path ?? getPath()
    // Record dedupe state so a router that calls page() alongside a pushState to
    // the same path doesn't also emit an auto pageview for it.
    this.lastViewPath = next
    this.lastViewTime = Date.now()
    this.currentViewId = newViewId()
    this._send(buildPageViewPayload(next))
    this.engagement?.reset(next)
  }

  /** Stop all tracking and clean up observers. */
  stop(): void {
    this.engagement?.flush()
    this.stopped = true
    for (const cleanup of this.cleanups) {
      cleanup()
    }
    this.cleanups = []
    this.engagement = null
  }
}
