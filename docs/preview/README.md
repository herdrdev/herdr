# Preview documentation

`website/` is the committed documentation snapshot for the active preview release recorded in `distribution/preview.json`.

Do not edit it manually. Preview CI replaces it from the selected commit's `docs/next/website` tree and commits the snapshot together with `distribution/preview.json`. Validate it with:

```bash
node scripts/docs/preview.mjs check
```
