// Only `pushState` is treated as a navigation. `replaceState` is intentionally
// not patched: SPA frameworks (Astro view transitions, React Router, Next,
// SvelteKit, Vue Router) call it during hydration/state-sync to normalize URLs
// without a real navigation, which would otherwise double-count the initial view.
//
// `hashchange` is intentionally NOT tracked: the recorded path has its hash
// stripped (privacy.ts), so an in-page anchor jump (/docs#a -> /docs#b) records
// the same path '/docs' twice — re-counting one page as many pageviews on
// docs/TOC-heavy sites. A true hash router (/#/a) likewise collapses to '/' once
// stripped, so hashchange can never produce a meaningful new path here.
export function startAutoTracking(onPageView: () => void): () => void {
  const pushState = history.pushState.bind(history)

  history.pushState = function (this: History, ...args: Parameters<typeof pushState>) {
    const result = pushState.apply(this, args)
    onPageView()
    return result
  }

  window.addEventListener('popstate', onPageView)

  return () => {
    history.pushState = pushState
    window.removeEventListener('popstate', onPageView)
  }
}
