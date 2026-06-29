import type { PerformanceMetrics } from './types'

export function startPerformanceTracking(
  report: (metrics: PerformanceMetrics) => void,
): () => void {
  let reported = false
  const observers: PerformanceObserver[] = []

  const metrics: Required<PerformanceMetrics> = {
    lcp: 0,
    fcp: 0,
    cls: 0,
    inp: 0,
    ttfb: 0,
  }

  // Per-interaction max latencies, keyed by interactionId; INP is derived from
  // these at flush time (see the INP observer below).
  const interactionMax = new Map<number, number>()

  // CLS is legitimately 0 on stable pages. Report it whenever the API exists
  // (even at 0) instead of only when > 0, so the server-side p75 isn't biased
  // upward by silently dropping every zero-shift page.
  const clsSupported =
    typeof PerformanceObserver !== 'undefined' &&
    (PerformanceObserver.supportedEntryTypes ?? []).includes('layout-shift')

  const onHidden = () => {
    if (document.visibilityState === 'hidden') flush()
  }
  let timer: ReturnType<typeof setTimeout> | null = null
  const cleanup = () => {
    document.removeEventListener('visibilitychange', onHidden)
    if (timer != null) clearTimeout(timer)
  }

  const flush = () => {
    if (reported) return
    reported = true
    observers.forEach((o) => o.disconnect())
    cleanup()

    if (interactionMax.size > 0) {
      const sorted = [...interactionMax.values()].sort((a, b) => b - a)
      // Drop ~1 worst interaction per 50; index 0 (the worst) for < 50.
      const idx = Math.min(sorted.length - 1, Math.floor(sorted.length / 50))
      metrics.inp = sorted[idx]!
    }

    const clean: PerformanceMetrics = {}
    if (metrics.lcp > 0) clean.lcp = Math.round(metrics.lcp)
    if (metrics.fcp > 0) clean.fcp = Math.round(metrics.fcp)
    if (clsSupported) clean.cls = Math.round(metrics.cls * 10000) / 10000
    if (metrics.inp > 0) clean.inp = Math.round(metrics.inp)
    if (metrics.ttfb > 0) clean.ttfb = Math.round(metrics.ttfb)

    if (Object.keys(clean).length > 0) report(clean)
  }

  // LCP
  try {
    const obs = new PerformanceObserver((list) => {
      const entries = list.getEntries()
      if (entries.length > 0) {
        metrics.lcp = entries[entries.length - 1]!.startTime
      }
    })
    obs.observe({ type: 'largest-contentful-paint', buffered: true })
    observers.push(obs)
  } catch {}

  // FCP
  try {
    const obs = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.name === 'first-contentful-paint') {
          metrics.fcp = entry.startTime
        }
      }
    })
    obs.observe({ type: 'paint', buffered: true })
    observers.push(obs)
  } catch {}

  // CLS
  let clsSession = 0
  let clsLastTime = 0
  try {
    const obs = new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as PerformanceEntry[]) {
        const shift = entry as unknown as {
          hadRecentInput?: boolean
          value?: number
        }
        if (shift.hadRecentInput) continue
        if (typeof shift.value !== 'number') continue

        if (clsLastTime === 0 || entry.startTime - clsLastTime < 1000) {
          clsSession += shift.value
        } else {
          metrics.cls = Math.max(metrics.cls, clsSession)
          clsSession = shift.value
        }
        clsLastTime = entry.startTime
      }
      metrics.cls = Math.max(metrics.cls, clsSession)
    })
    obs.observe({ type: 'layout-shift', buffered: true })
    observers.push(obs)
  } catch {}

  // INP — Interaction to Next Paint. A single interaction (e.g. tap) emits
  // several event-timing entries (pointerdown, pointerup, click) sharing one
  // interactionId; the interaction's latency is the max duration among them.
  // INP is then a high percentile of per-interaction latencies that discards
  // ~1 outlier per 50 interactions (the web-vitals definition) — the worst
  // single interaction for short sessions, but trimmed on busy pages so one
  // outlier tap doesn't inflate the reported value (and the server-side p75).
  try {
    const obs = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const id = (entry as PerformanceEntry & { interactionId?: number })
          .interactionId
        if (!id) continue // 0/undefined => not a discrete interaction
        const prev = interactionMax.get(id) ?? 0
        if (entry.duration > prev) interactionMax.set(id, entry.duration)
      }
    })
    obs.observe({
      type: 'event',
      durationThreshold: 40,
      buffered: true,
    } as PerformanceObserverInit & { durationThreshold: number })
    observers.push(obs)
  } catch {}

  // TTFB — try sync read first, then observe in case nav entry isn't ready yet
  const readTtfb = (e?: PerformanceNavigationTiming) => {
    const nav =
      e ??
      (performance.getEntriesByType(
        'navigation',
      )[0] as PerformanceNavigationTiming | undefined)
    if (nav && nav.responseStart > 0 && metrics.ttfb === 0) {
      metrics.ttfb = nav.responseStart
    }
  }
  try {
    readTtfb()
    const obs = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        readTtfb(entry as PerformanceNavigationTiming)
      }
    })
    obs.observe({ type: 'navigation', buffered: true })
    observers.push(obs)
  } catch {}

  document.addEventListener('visibilitychange', onHidden)
  timer = setTimeout(flush, 15000)

  return () => {
    flush()
  }
}
