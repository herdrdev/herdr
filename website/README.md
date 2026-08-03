# herdr website

The homepage is `index.html`. Astro Starlight renders the documentation.

```bash
bun install
bun run dev          # render the unpublished docs/next draft locally
bun run build        # render only published stable and preview snapshots
bun run build:draft  # validate the unpublished draft
```

The build output is `dist/`. Configure Cloudflare Pages to use `website` as the project root and publish `dist`.

Documentation has three lifecycle states:

- `../docs/next/website/` is the committed, author-edited draft. Production builds never read it.
- `../docs/preview/website/` is the latest preview release snapshot, rendered at `/docs/preview/`.
- `../docs/versions/<version>/website/` contains immutable stable release snapshots, rendered at `/docs/<version>/`.

The current stable site temporarily remains tracked under `src/content/docs/` because it contains post-v0.7.5 documentation corrections. The next stable release switches `docs/versions/manifest.json` to snapshot-backed stable docs and removes that legacy copy. From then on, `src/content/docs/` is entirely generated and ignored.

Preview CI snapshots the selected commit and updates `preview.json` in one commit:

```bash
node website/scripts/docs-preview.mjs snapshot <commit>
node website/scripts/docs-preview.mjs check
```

Stable release CI snapshots the exact tag after the GitHub Release succeeds:

```bash
node website/scripts/docs-versions.mjs publish <tag>
node website/scripts/docs-versions.mjs check
```
