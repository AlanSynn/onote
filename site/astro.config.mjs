// @ts-check
import { defineConfig } from 'astro/config';

// GitHub Pages subpath setup — LOAD-BEARING.
//
// `alansynn.com` is the apex, owned by the user-site (AlanSynn/alansynn.github.io,
// cname=alansynn.com, build_type=workflow). This project repo therefore serves at
// the `/onote/` subpath automatically. Astro composes the final URL as
// `site + base`, so:
//   - `site` MUST be the apex, NOT `.../onote` (else canonical/Astro.url mis-resolve).
//   - `base` is the subpath with NO trailing slash (Astro + GH Pages convention).
//   - `trailingSlash: 'never'` + `build.format: 'directory'` → clean URLs
//     (`/onote/manual/install` → dist/manual/install/index.html).
//
// Footgun: root-absolute markdown links like `/manual/editor` are left UN-prefixed
// by Astro and 404 under the subpath. Write RELATIVE links in markdown
// (`./editor.md`) only. scripts/build-downloads.mjs lints dist for stray
// `href="/manual` and fails the build if any slip through.
export default defineConfig({
  site: 'https://alansynn.com',
  base: '/onote',
  trailingSlash: 'never',
  build: { format: 'directory' },
  devToolbar: { enabled: false },
});
