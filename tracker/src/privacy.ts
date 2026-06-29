import type { Campaign } from './types'

export function stripQueryParams(path: string): string {
  const q = path.indexOf('?')
  const h = path.indexOf('#')
  const cuts = [q, h].filter((i) => i !== -1)
  if (cuts.length === 0) return path
  return path.slice(0, Math.min(...cuts))
}

export function getReferrerDomain(): string | undefined {
  try {
    if (!document.referrer) return undefined
    const url = new URL(document.referrer)
    if (url.hostname === location.hostname) return undefined
    return url.hostname
  } catch {
    return undefined
  }
}

export function getDeviceClass(w: number): 'mobile' | 'tablet' | 'desktop' {
  if (w < 640) return 'mobile'
  if (w < 1024) return 'tablet'
  return 'desktop'
}

export function roundViewportWidth(w: number): number {
  return Math.round(w / 10) * 10
}

const UTM_MAX = 128

export function extractCampaign(search: string): Campaign | undefined {
  let params: URLSearchParams
  try {
    params = new URLSearchParams(search)
  } catch {
    return undefined
  }
  const pick = (key: string): string | undefined => {
    const v = params.get(key)
    if (!v) return undefined
    return v.length > UTM_MAX ? v.slice(0, UTM_MAX) : v
  }
  const s = pick('utm_source')
  const m = pick('utm_medium')
  const c = pick('utm_campaign')
  if (s === undefined && m === undefined && c === undefined) return undefined
  const out: Campaign = {}
  if (s !== undefined) out.s = s
  if (m !== undefined) out.m = m
  if (c !== undefined) out.c = c
  return out
}

export function checkDNT(): boolean {
  const nav = navigator as Navigator & { globalPrivacyControl?: boolean }
  const win = window as Window & { doNotTrack?: string }
  return (
    navigator.doNotTrack === '1' ||
    nav.globalPrivacyControl === true ||
    win.doNotTrack === '1'
  )
}
