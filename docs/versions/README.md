# Versioned documentation

This directory contains the published documentation for stable Herdr releases.

Release CI creates each version from the tagged `docs/next` tree after the GitHub Release succeeds. Maintainers can correct published documentation in its version directory afterward. When a correction also applies to future releases, make the same focused change under `docs/next`; do not replace a published tree with the current draft.

Validate every published version with:

```bash
node scripts/docs/versions.mjs check
```

The private website renders each maintained version at `/docs/<version>/`, uses the version selected by `manifest.json` for `/docs/`, and renders the active preview snapshot at `/docs/preview/`. The source snapshots in this directory remain public release evidence; the private repository owns only their presentation.

The `tag`, `commit`, and `source` fields in `manifest.json` record where release CI initially published a version. Git history records later documentation corrections.

The historical backfill starts at v0.5.11, the first release that included the Astro/Starlight documentation site.
