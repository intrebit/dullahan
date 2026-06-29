const MILESTONES = [25, 50, 75, 100] as const

export interface ScrollTracker {
  reset(): void
  stop(): void
}

function scheduleFrame(cb: () => void): number {
  if (typeof requestAnimationFrame === 'function') {
    return requestAnimationFrame(cb)
  }
  cb()
  return -1
}

function docHeight(): number {
  const d = document.documentElement
  const b = document.body
  return Math.max(
    d ? d.scrollHeight : 0,
    d ? d.offsetHeight : 0,
    d ? d.clientHeight : 0,
    b ? b.scrollHeight : 0,
    b ? b.offsetHeight : 0,
  )
}

// Emits scroll-depth milestones (25/50/75/100) once each per page view. The
// caller fires a `scroll_depth` event with the milestone percent. rAF-throttled
// so a burst of scroll events collapses to one measurement per frame.
export function startScrollTracking(emit: (pct: number) => void): ScrollTracker {
  let fired = new Set<number>()
  let scheduled = false
  let rafId = -1

  const measure = () => {
    scheduled = false
    rafId = -1
    const dh = docHeight()
    if (dh <= 0) return
    const depth = (window.scrollY + window.innerHeight) / dh
    const pct = Math.min(100, Math.round(depth * 100))
    for (const m of MILESTONES) {
      if (pct >= m && !fired.has(m)) {
        fired.add(m)
        emit(m)
      }
    }
  }

  const onScroll = () => {
    if (scheduled) return
    scheduled = true
    rafId = scheduleFrame(measure)
  }

  const cancelPending = () => {
    if (rafId !== -1 && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(rafId)
    }
    rafId = -1
    scheduled = false
  }

  window.addEventListener('scroll', onScroll, { passive: true })
  // Short pages may already show their footer — measure once on start so they
  // are not stuck reporting 0% until a (never-coming) scroll.
  measure()

  return {
    // SPA route change: forget which milestones fired. We deliberately do not
    // re-measure here — at reset time the new page may not be laid out and the
    // scroll position may still be the old page's, which would fire bogus
    // milestones. The next real scroll measures against the new page. Cancel any
    // rAF measure queued by a scroll just before the navigation for the same
    // reason — it would run against the outgoing page and emit on the new view.
    reset() {
      cancelPending()
      fired = new Set()
    },
    stop() {
      cancelPending()
      window.removeEventListener('scroll', onScroll)
    },
  }
}
