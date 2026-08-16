import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';

await patchFullBundleExclusions();
await disablePluginIndexRoute();

async function patchFullBundleExclusions() {
  const path = resolve('node_modules/starlight-llms-txt/llms-full.txt.ts');
  const original = await readFile(path, 'utf8');
  const importLine =
    "import { starlightLllmsTxtContext } from 'virtual:starlight-llms-txt/context';";
  const excludeLine = '\t\texclude: starlightLllmsTxtContext.exclude,';

  if (original.includes(importLine) && original.includes(excludeLine)) return;

  const withImport = original.replace(
    "import { generateLlmsTxt } from './generator';",
    `${importLine}\nimport { generateLlmsTxt } from './generator';`,
  );
  const patched = withImport.replace(
    '\t\tdescription: `This is the full developer documentation for ${getSiteTitle()}`,',
    '\t\tdescription: `This is the full developer documentation for ${getSiteTitle()}`,\n' +
      excludeLine,
  );

  if (patched === original || !patched.includes(importLine) || !patched.includes(excludeLine)) {
    throw new Error(
      'starlight-llms-txt changed: update the stable-only llms-full compatibility patch',
    );
  }

  await writeFile(path, patched, 'utf8');
}

async function disablePluginIndexRoute() {
  const path = resolve('node_modules/starlight-llms-txt/index.ts');
  const original = await readFile(path, 'utf8');
  const marker =
    '\t\t\t\t\t\t\t// Herdr publishes channel-aware raw-source indexes from website/public.';
  if (original.includes(marker)) return;

  const route = `\t\t\t\t\t\t\tinjectRoute({
\t\t\t\t\t\t\t\tentrypoint: new URL('./llms.txt.ts', import.meta.url),
\t\t\t\t\t\t\t\tpattern: '/llms.txt',
\t\t\t\t\t\t\t});`;
  const patched = original.replace(route, marker);
  if (patched === original || !patched.includes(marker)) {
    throw new Error(
      'starlight-llms-txt changed: update the custom llms.txt index compatibility patch',
    );
  }

  await writeFile(path, patched, 'utf8');
}
