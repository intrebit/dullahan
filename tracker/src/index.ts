import type { AnalyticsConfig, Payload, PayloadInput, PerformanceMetrics } from './types'
import { checkDNT } from './privacy'
import { sendPayload } from './transport'
import {
  buildPageViewPayload,
  buildEventPayload,
  buildPerformancePayload,
  buildPageLeavePayload,
  getPath,
} from './payload'
import { startAutoTracking } from './collect'
import { startPerformanceTracking } from './performance'
import { startEngagement, type Engagement } from './engagement'
import { startScrollTracking, type ScrollTracker } from './scroll'
import { startClickTracking } from './clicks'

export type {
  AnalyticsConfig,
  Payload,
  PayloadInput,
  PageviewPayload,
  EventPayload,
  PerformancePayload,
  PageleavePayload,
  PerformanceMetrics,
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
  private scroll: ScrollTracker | null = null
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
      trackScroll: config.trackScroll ?? false,
      trackOutboundLinks: config.trackOutboundLinks ?? false,
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

    this._startPerformanceTracking()
  }

  private _send(payload: PayloadInput): void {
    if (this.stopped) return
    const full = { ...payload, s: this.config.siteId } as Payload
    // A payload may carry an explicit vid (performance metrics pin the view
    // they measured); only fall back to the live view id when it doesn't.
    if (full.vid === undefined && this.currentViewId) full.vid = this.currentViewId
    sendPayload(full, this.config.endpoint)
  }

  private _startAutoTracking(): void {
    const eng = startEngagement((path, dur) => {
      this._send(buildPageLeavePayload(path, dur))
    })
    this.engagement = eng
    this.cleanups.push(() => eng.stop())

    if (this.config.trackScroll) {
      const scroll = startScrollTracking((pct) => {
        this._send(buildEventPayload('scroll_depth', { pct }))
      })
      this.scroll = scroll
      this.cleanups.push(() => scroll.stop())
    }

    if (this.config.trackOutboundLinks) {
      const clicks = startClickTracking((name, props) => {
        this._send(buildEventPayload(name, props))
      })
      this.cleanups.push(() => clicks.stop())
    }

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
      this.scroll?.reset()
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

  private _startPerformanceTracking(): void {
    // Web-vitals flush asynchronously (a 15s timeout or the first tab-hide),
    // potentially long after an SPA navigation has regenerated currentViewId.
    // Pin the view id and path of the pageload being measured now so the
    // metrics aren't misattributed to a later page-visit.
    const viewId = this.currentViewId
    const path = getPath()
    const send = (metrics: PerformanceMetrics) => {
      const payload = buildPerformancePayload(metrics)
      if (viewId) {
        payload.vid = viewId
        payload.p = path
      }
      this._send(payload)
    }
    this.cleanups.push(startPerformanceTracking(send))
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
    this.scroll?.reset()
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
    this.scroll = null
  }
}
