# Preview documentation

`website/` is the committed documentation snapshot for the active preview release in `website/preview.json`.

Do not edit it manually. Preview CI replaces it from the selected commit's `docs/next/website` tree and commits the snapshot together with `website/preview.json`. Validate it with:

```bash
node website/scripts/docs-preview.mjs check
```
