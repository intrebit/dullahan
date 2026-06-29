import type { Payload } from './types'

export function sendPayload(payload: Payload, endpoint: string): void {
  const body = JSON.stringify(payload)
  const blob = new Blob([body], { type: 'application/json' })

  try {
    if (navigator.sendBeacon) {
      try {
        if (navigator.sendBeacon(endpoint, blob)) return
      } catch {
        // sendBeacon can throw (not only return false) when the body exceeds the
        // per-origin queue limit; fall through to fetch rather than dropping it.
      }
    }

    fetch(endpoint, {
      method: 'POST',
      body: blob,
      keepalive: true,
    }).catch(() => {})
  } catch {
    // silently ignore failures — analytics must never break the page
  }
}
