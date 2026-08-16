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
- `../docs/versions/<version>/website/` contains maintained stable-release documentation, rendered at `/docs/<version>/`. Release CI seeds each new version from its tag. Make later factual corrections directly in that version directory, and mirror them to `docs/next` when they also apply to future releases.

The version selected by `docs/versions/manifest.json` is also rendered at `/docs/`. `src/content/docs/` is entirely generated and ignored.

Preview CI snapshots the selected commit and updates `preview.json` in one commit:

```bash
node website/scripts/docs-preview.mjs snapshot <commit>
node website/scripts/docs-preview.mjs check
```

Stable release CI seeds a new maintained version from the exact tag after the GitHub Release succeeds:

```bash
node website/scripts/docs-versions.mjs publish <tag>
node website/scripts/docs-versions.mjs check
```
