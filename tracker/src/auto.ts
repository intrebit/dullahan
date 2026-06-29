// Self-initializing entry for the server-served <script> tag. Unlike index.ts
// (a library with no side effects), this runs on load: it reads config from its
// own <script data-*> attributes and starts tracking, so adopters need no npm
// install or build step — just one line of HTML:
//
//   <script defer src="https://analytics.example.com/pt.js" data-site="my-site"></script>
//
// data-endpoint defaults to /collect on the origin that served this script.
// Boolean opt-ins: data-track-scroll, data-track-outbound, data-respect-dnt.
// data-auto-track="false" disables automatic pageviews.
import { Analytics } from './index'

interface DullahanGlobal {
  track: (name: string, props?: Record<string, unknown>) => void
  page: (path?: string) => void
  stop: () => void
}

function findScriptEl(): HTMLScriptElement | null {
  const current = document.currentScript
  if (current instanceof HTMLScriptElement && current.dataset.site) {
    return current
  }
  // currentScript is null for module/async scripts; fall back to the first
  // dullahan tag on the page.
  return document.querySelector<HTMLScriptElement>('script[data-site]')
}

// An attribute is "on" when present and not explicitly "false".
function flag(el: HTMLScriptElement, key: string): boolean {
  return key in el.dataset && el.dataset[key] !== 'false'
}

try {
  const el = findScriptEl()
  if (!el) {
    console.warn('dullahan: no <script data-site="..."> found; tracking not started')
  } else {
    const instance = new Analytics({
      siteId: el.dataset.site ?? '',
      endpoint: el.dataset.endpoint || new URL('/collect', el.src).href,
      autoTrack:
        el.dataset.autoTrack === undefined ? undefined : el.dataset.autoTrack !== 'false',
      respectDNT: flag(el, 'respectDnt'),
      trackScroll: flag(el, 'trackScroll'),
      trackOutboundLinks: flag(el, 'trackOutbound'),
    })

    const w = window as unknown as { dullahan?: DullahanGlobal }
    w.dullahan = {
      track: (name, props) => instance.track(name, props),
      page: (path) => instance.page(path),
      stop: () => instance.stop(),
    }
  }
} catch (err) {
  // Analytics must never break the host page.
  if (typeof console !== 'undefined') {
    console.warn('dullahan: failed to start', err)
  }
}
