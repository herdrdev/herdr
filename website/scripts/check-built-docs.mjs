import { access, readFile, readdir, stat } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const websiteDir = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const distDir = resolve(websiteDir, 'dist');
const nonCanonicalDocsUrl = /https:\/\/herdr\.dev\/(?:ja\/|zh-cn\/)?docs\/(?:preview|\d+\.\d+\.\d+)(?:\/|<)/;
const versions = JSON.parse(
  await readFile(resolve(websiteDir, 'src/data/docs-versions.json'), 'utf8'),
);

for (const entry of versions.versions) {
  const scope = versions.scopes[entry.version];
  if (!scope) throw new Error(`missing generated scope for ${entry.version}`);
  for (const [locale, pages] of Object.entries(scope.locales)) {
    const localePrefix = locale === 'root' ? '' : `${locale}/`;
    for (const page of pages) {
      const output = resolve(
        distDir,
        localePrefix,
        'docs',
        entry.version,
        page,
        'index.html',
      );
      await access(output);
    }
  }
}

const stable = await readFile(resolve(distDir, 'docs/index.html'), 'utf8');
const preview = await readFile(resolve(distDir, 'docs/preview/index.html'), 'utf8');
const archived = await readFile(
  resolve(distDir, 'docs', versions.current, 'index.html'),
  'utf8',
);

assertIncludes(stable, 'data-pagefind-filter="version[content]" content="stable"');
if (stable.includes('name="robots" content="noindex')) {
  throw new Error('stable docs must remain indexable');
}
assertIncludes(preview, 'data-pagefind-filter="version[content]" content="preview"');
assertIncludes(preview, `Preview build <code dir="auto">${versions.preview.build_id}</code>`);
if (versions.preview.commit !== 'master') {
  assertIncludes(preview, versions.preview.commit.slice(0, 12));
}
assertIncludes(preview, 'name="robots" content="noindex, nofollow"');
assertIncludes(archived, `data-pagefind-filter="version[content]" content="${versions.current}"`);
assertIncludes(archived, 'name="robots" content="noindex, nofollow"');
if (archived.includes(`This page documents Herdr ${versions.current}`)) {
  throw new Error('the current documentation version must not be labeled as outdated');
}
const versionSelect = stable.match(/<select[^>]*aria-label="Documentation version"[^>]*>([\s\S]*?)<\/select>/)?.[1];
if (!versionSelect) throw new Error('stable docs are missing the version selector');
if (versionSelect.includes(`value="/docs/${versions.current}/"`)) {
  throw new Error('the current release is duplicated in the version selector');
}

const previous = versions.versions.find((entry) => entry.version !== versions.current);
if (previous) {
  const previousArchive = await readFile(
    resolve(distDir, 'docs', previous.version, 'index.html'),
    'utf8',
  );
  assertIncludes(previousArchive, `This page documents Herdr ${previous.version}`);
}

const sitemap = await readFile(resolve(distDir, 'sitemap-0.xml'), 'utf8');
assertIncludes(sitemap, 'https://herdr.dev/docs/');
if (nonCanonicalDocsUrl.test(sitemap)) {
  throw new Error('preview or versioned documentation URLs must not appear in the sitemap');
}

const llmsIndex = await readFile(resolve(distDir, 'llms.txt'), 'utf8');
const llmsPreview = await readFile(resolve(distDir, 'llms-preview.txt'), 'utf8');
const llmsSmall = await readFile(resolve(distDir, 'llms-small.txt'), 'utf8');
const llmsFull = await readFile(resolve(distDir, 'llms-full.txt'), 'utf8');
const headers = await readFile(resolve(distDir, '_headers'), 'utf8');
const currentDocs = versions.versions.find((entry) => entry.version === versions.current);
if (!currentDocs?.tag || !currentDocs.source) {
  throw new Error(`current docs version ${versions.current} is missing its tag or source`);
}
const stableRawRoot = `https://raw.githubusercontent.com/herdrdev/herdr/${currentDocs.tag}`;
const stableRawBase = `${stableRawRoot}/${currentDocs.source}`;
const previewRawBase = `https://raw.githubusercontent.com/herdrdev/herdr/${versions.preview.commit}/docs/next/website/src/content/docs`;
assertIncludes(llmsIndex, `Current stable release: ${versions.current}.`);
assertIncludes(llmsIndex, `${stableRawBase}/quick-start.mdx`);
assertIncludes(
  llmsIndex,
  `${stableRawRoot}/${currentDocs.source.replace(/\/content\/docs$/, '/data/config-reference.json')}`,
);
assertIncludes(llmsIndex, 'https://herdr.dev/llms-small.txt');
assertIncludes(llmsIndex, 'https://herdr.dev/llms-full.txt');
assertIncludes(llmsIndex, 'https://herdr.dev/agent-guide.md');
assertIncludes(llmsIndex, 'https://herdr.dev/llms-preview.txt');
assertIncludes(llmsPreview, `Active preview build: ${versions.preview.build_id}`);
assertIncludes(llmsPreview, `${previewRawBase}/quick-start.mdx`);
assertIncludes(llmsPreview, 'https://herdr.dev/llms.txt');
const stablePageLinks = llmsIndex
  .split('\n')
  .filter((line) => line.includes(`](${stableRawBase}/`));
if (stablePageLinks.length !== versions.scopes.stable.locales.root.length) {
  throw new Error(
    `llms.txt lists ${stablePageLinks.length} stable pages, expected ${versions.scopes.stable.locales.root.length}`,
  );
}
const previewPageLinks = llmsPreview
  .split('\n')
  .filter((line) => line.includes(`](${previewRawBase}/`));
if (previewPageLinks.length !== versions.scopes.preview.locales.root.length) {
  throw new Error(
    `llms-preview.txt lists ${previewPageLinks.length} preview pages, expected ${versions.scopes.preview.locales.root.length}`,
  );
}
for (const path of ['/llms.txt', '/llms-preview.txt', '/llms-small.txt', '/llms-full.txt']) {
  assertIncludes(headers, `${path}\n  Content-Type: text/markdown; charset=utf-8`);
}
for (const [name, content] of [
  ['llms-small.txt', llmsSmall],
  ['llms-full.txt', llmsFull],
]) {
  assertIncludes(content, '# Herdr documentation');
  assertIncludes(content, '# Troubleshooting');
  assertIncludes(content, '# Socket API');
  if (content.match(/^# Herdr documentation$/gm)?.length !== 1) {
    throw new Error(`${name} must contain exactly one stable documentation set`);
  }
  if (/\]\(\/docs\/(?:preview|\d+\.\d+\.\d+)(?:\/|\))/.test(content)) {
    throw new Error(`${name} must not link to preview or versioned documentation`);
  }
}

const build = await inspectFiles(distDir);
if (build.count > 20_000) {
  throw new Error(`website build has ${build.count} files, exceeding the Cloudflare Pages free-plan limit`);
}
if (build.largest.bytes > 25 * 1024 * 1024) {
  throw new Error(`website asset ${build.largest.path} is ${build.largest.bytes} bytes, exceeding Cloudflare Pages' 25 MiB limit`);
}
process.stdout.write(`validated ${versions.versions.length} documentation versions in ${build.count} website files\n`);

function assertIncludes(content, expected) {
  if (!content.includes(expected)) throw new Error(`built documentation is missing ${expected}`);
}

async function inspectFiles(directory, root = directory) {
  const result = { count: 0, largest: { path: '', bytes: 0 } };
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      const nested = await inspectFiles(path, root);
      result.count += nested.count;
      if (nested.largest.bytes > result.largest.bytes) result.largest = nested.largest;
    } else if (entry.isFile()) {
      result.count += 1;
      const { size } = await stat(path);
      if (size > result.largest.bytes) {
        result.largest = { path: path.slice(root.length + 1), bytes: size };
      }
    }
  }
  return result;
}
