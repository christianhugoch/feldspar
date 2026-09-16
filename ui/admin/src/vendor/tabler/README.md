# Tabler (vendored)

`tabler.min.css` is the compiled stylesheet from **Tabler v1.5.1**
(<https://tabler.io>), the theme the admin UI is built on. It is checked in
rather than pulled from npm or a CDN for two reasons:

- the admin bundle is served under a strict CSP (`style-src 'self'`), so every
  byte of CSS has to be same-origin — a CDN link is not an option;
- the file is entirely self-contained. Every image in it is a `data:` URI and
  its font stack is the system UI font, so no request leaves the origin at
  render time. (Tabler stopped bundling a webfont in 1.5.0; before that its
  templates `@import`ed Inter from `rsms.me`, and that line was never
  reproduced here.)

Tabler's CSS is a superset of Bootstrap 5's, which is why the screens keep using
`react-bootstrap` components — they emit Bootstrap markup, and this stylesheet
is what makes it look like Tabler. `bootstrap`'s own stylesheet is therefore not
imported; the npm package stays a dependency only because `react-bootstrap`
declares it as a peer.

Only the CSS is vendored. Tabler also ships `tabler.min.js` (Bootstrap's JS
plus a theme script), which this SPA does not use: React owns the DOM, so the
sidebar collapse is component state and the light/dark switch is the
`data-bs-theme` attribute set from `layout.tsx`'s `useTheme`. The folded sidebar
is the same story — `App.tsx` puts Tabler's `navbar-folded-hover` class on the
aside directly, where a static Tabler page would let `tabler-theme.js` write
`data-bs-sidebar` on `<html>` from `localStorage`. The one place that shows is
`[data-bs-toggle="sidebar-folded"]` on the pin button: it is there for the
*styling* Tabler hangs off that attribute (hidden while the rail is folded,
faded in on hover), not for the JS, and React handles the click.

Licensed under MIT — Copyright 2018-2026 The Tabler Authors, copyright
2018-2026 codecalm.net Paweł Kuna. The notice is preserved in the banner comment
at the top of `tabler.min.css`; the full text is at
<https://github.com/tabler/tabler/blob/master/LICENSE>.

**Upgrading:** replace `tabler.min.css` with `dist/css/tabler.min.css` from the
new release and re-run the admin build. Nothing else here is generated. Note
that the GitHub release *zip* is the source tree — it has no `dist/`, only
`core/scss` — so the built file comes from the npm package:
`npm pack @tabler/core@<version>` and take `package/dist/css/tabler.min.css`
from the tarball. `crates/sc-server/tests/admin_theme.rs` checks the properties
above, so a release that reintroduces an `@import` or a remote `url()` fails the
test suite rather than the browser.

The Saltcorn logo is vendored one directory up (`../saltcorn-logo.svg`) — it is
ours, not Tabler's.
