# Versioned documentation

This directory contains immutable documentation snapshots for stable Herdr releases.

Do not edit snapshot files manually. They must match the release tag recorded in `manifest.json`. Validate them with:

```bash
node website/scripts/docs-versions.mjs check
```

Release CI creates a new snapshot from the tagged `docs/next` tree after the GitHub Release succeeds. `website/scripts/prepare-docs.mjs` renders these snapshots at `/docs/<version>/` and uses the current snapshot for `/docs/` after the legacy stable-doc migration completes. `/docs/preview/` comes only from the active preview release snapshot in `docs/preview/`, never directly from `docs/next/`.

The historical backfill starts at v0.5.11, the first release that included the Astro/Starlight documentation site.
