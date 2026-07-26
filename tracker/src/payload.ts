import type { PageviewPayload, EventPayload, PageleavePayload } from './types'
import {
  stripQueryParams,
  getReferrerDomain,
  getDeviceClass,
  extractCampaign,
} from './privacy'

export function getPath(): string {
  return stripQueryParams(location.pathname + location.search)
}

export function buildPageViewPayload(path?: string): Omit<PageviewPayload, 's'> {
  const u = extractCampaign(location.search)

  return {
    t: 'pageview',
    p: path ? stripQueryParams(path) : getPath(),
    ts: Date.now(),
    r: getReferrerDomain(),
    d: getDeviceClass(window.innerWidth),
    ...(u ? { u } : {}),
  }
}

export function buildEventPayload(
  name: string,
  props?: Record<string, unknown>,
): Omit<EventPayload, 's'> {
  return {
    t: 'event',
    p: getPath(),
    ts: Date.now(),
    n: name,
    ...(props && Object.keys(props).length > 0 ? { pr: props } : {}),
  }
}

export function buildPageLeavePayload(path: string, dur: number): Omit<PageleavePayload, 's'> {
  return {
    t: 'pageleave',
    p: stripQueryParams(path),
    ts: Date.now(),
    dur,
  }
}
