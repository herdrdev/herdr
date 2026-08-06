# Versioned documentation

This directory contains the published documentation for stable Herdr releases.

Release CI creates each version from the tagged `docs/next` tree after the GitHub Release succeeds. Maintainers can correct published documentation in its version directory afterward. When a correction also applies to future releases, make the same focused change under `docs/next`; do not replace a published tree with the current draft.

Validate the manifest and build every published version with:

```bash
node website/scripts/docs-versions.mjs check
cd website && bun run build
```

`website/scripts/prepare-docs.mjs` renders each maintained version at `/docs/<version>/` and uses the version selected by `manifest.json` for `/docs/`. Generated files under `website/src/content/docs/` are not editable sources. `/docs/preview/` comes only from the active preview release snapshot in `docs/preview/`, never directly from `docs/next/`.

The `tag`, `commit`, and `source` fields in `manifest.json` record where release CI initially published a version. Git history records later documentation corrections.

The historical backfill starts at v0.5.11, the first release that included the Astro/Starlight documentation site.
