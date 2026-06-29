export interface AnalyticsConfig {
  endpoint: string
  siteId: string
  autoTrack?: boolean
  respectDNT?: boolean
  trackScroll?: boolean
  trackOutboundLinks?: boolean
}

export interface Campaign {
  s?: string
  m?: string
  c?: string
}

export type EventType = 'pageview' | 'event' | 'performance' | 'pageleave'

export type PerformanceMetrics = {
  lcp?: number
  fcp?: number
  cls?: number
  inp?: number
  ttfb?: number
}

/** Fields present on every event regardless of type. */
interface BasePayload {
  /** Site id. */
  s: string
  /** Path (query + hash stripped). */
  p: string
  /** Client timestamp (epoch ms). */
  ts: number
  /** Per-pageload view id; attached to every event of a pageload. */
  vid?: string
}

export interface PageviewPayload extends BasePayload {
  t: 'pageview'
  r?: string
  d?: 'mobile' | 'tablet' | 'desktop'
  v?: number
  u?: Campaign
}

export interface EventPayload extends BasePayload {
  t: 'event'
  n: string
  pr?: Record<string, unknown>
}

export interface PerformancePayload extends BasePayload {
  t: 'performance'
  pf: PerformanceMetrics
}

export interface PageleavePayload extends BasePayload {
  t: 'pageleave'
  dur: number
}

/**
 * The wire payload sent to `/collect`. A discriminated union on `t`, mirroring
 * the server's tagged `RawPayload` enum: each event type carries exactly the
 * fields the server requires for it, so the type can't describe a payload the
 * server would reject (e.g. an `event` with no `n`).
 */
export type Payload =
  | PageviewPayload
  | EventPayload
  | PerformancePayload
  | PageleavePayload

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown
  ? Omit<T, K>
  : never

/** A built payload before the Analytics instance stamps the site id (`s`). */
export type PayloadInput = DistributiveOmit<Payload, 's'>
