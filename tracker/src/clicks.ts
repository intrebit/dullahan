import { stripQueryParams } from './privacy'

export interface ClickTracker {
  stop(): void
}

const DOWNLOAD_EXTS = [
  'pdf',
  'zip',
  'gz',
  'tar',
  'rar',
  '7z',
  'dmg',
  'pkg',
  'exe',
  'csv',
  'xls',
  'xlsx',
  'doc',
  'docx',
  'ppt',
  'pptx',
  'mp3',
  'mp4',
  'mov',
  'wav',
  'avi',
]

function hasDownloadExt(pathname: string): boolean {
  const dot = pathname.lastIndexOf('.')
  if (dot === -1) return false
  return DOWNLOAD_EXTS.includes(pathname.slice(dot + 1).toLowerCase())
}

// One delegated listener catches outbound-link and download clicks. The caller
// fires an `outbound` or `download` event with the destination (query/hash
// stripped). Same-origin navigations are ignored — they show up as pageviews.
export function startClickTracking(
  emit: (name: string, props: Record<string, unknown>) => void,
): ClickTracker {
  const onClick = (e: MouseEvent) => {
    const target = e.target
    if (!(target instanceof Element)) return
    const anchor = target.closest('a')
    const href = anchor?.getAttribute('href')
    if (!anchor || !href) return

    // Resolve from the raw attribute, not `anchor.href`: on an SVG <a> the latter
    // is an SVGAnimatedString that stringifies to garbage ("[object …]"), which
    // would misclassify or drop SVG (icon/logo) link clicks.
    let url: URL
    try {
      url = new URL(href, location.href)
    } catch {
      return
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return

    const dest = stripQueryParams(url.href)
    if (anchor.hasAttribute('download') || hasDownloadExt(url.pathname)) {
      emit('download', { href: dest })
    } else if (url.host !== location.host) {
      emit('outbound', { href: dest })
    }
  }

  document.addEventListener('click', onClick, true)

  return {
    stop() {
      document.removeEventListener('click', onClick, true)
    },
  }
}
