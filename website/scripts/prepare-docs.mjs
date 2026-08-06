import { cp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = process.env.HERDR_DOCS_REPO_ROOT
  ? resolve(process.env.HERDR_DOCS_REPO_ROOT)
  : resolve(websiteDir, '../..');
const publicDir = resolve(repoRoot, 'website/public');
const stableDocsDir = resolve(repoRoot, 'website/src/content/docs');
const previewDocsDir = resolve(stableDocsDir, 'preview');
const generatedVersionsDocsDir = resolve(stableDocsDir, '_versions');
const versionsDir = resolve(repoRoot, 'docs/versions');
const versionsManifestPath = resolve(versionsDir, 'manifest.json');
const previewManifestPath = resolve(repoRoot, 'website/preview.json');
const generatedVersionsDataPath = resolve(repoRoot, 'website/src/data/docs-versions.json');
const stableConfigReferenceDestination = resolve(
  repoRoot,
  'website/src/data/config-reference.json',
);
const previewConfigReferenceDestination = resolve(
  repoRoot,
  'website/src/data/config-reference-preview.json',
);
const generatedVersionReferencesPath = resolve(
  repoRoot,
  'website/src/data/config-reference-versions.json',
);

if (process.argv[2] === '--rewrite-preview-doc-fixture') {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  process.stdout.write(
    rewritePreviewDocContent(Buffer.concat(chunks).toString('utf8'), '', {
      buildId: '2026-07-29-44b3adb12552',
      commit: '44b3adb125524ea9a55739eee3776f922f2115ad',
    }),
  );
} else if (process.argv[2] === '--rewrite-version-doc-fixture') {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  process.stdout.write(
    rewriteVersionDocContent(Buffer.concat(chunks).toString('utf8'), {
      version: process.argv[3] ?? '0.7.5',
      tag: `v${process.argv[3] ?? '0.7.5'}`,
      relativePath: 'index.mdx',
    }),
  );
} else {
  const supported = new Set(['--docs-only', '--draft']);
  const unsupported = process.argv.slice(2).filter((argument) => !supported.has(argument));
  if (unsupported.length > 0) {
    throw new Error('usage: node website/scripts/prepare-docs.mjs [--docs-only] [--draft]');
  }
  if (!process.argv.includes('--docs-only')) await preparePublicAssets();
  await prepareDocs({ draft: process.argv.includes('--draft') });
}

async function preparePublicAssets() {
  await rm(publicDir, { recursive: true, force: true });
  await mkdir(publicDir, { recursive: true });

  for (const file of [
    'install.sh',
    'install.ps1',
    'agent-guide.md',
    'latest.json',
    'preview.json',
    'robots.txt',
    '_headers',
    '_redirects',
  ]) {
    const source = resolve(repoRoot, 'website', file);
    try {
      await cp(source, resolve(publicDir, file));
    } catch (error) {
      if (file !== 'preview.json' || error.code !== 'ENOENT') throw error;
    }
  }

  for (const directory of ['assets', 'css', 'agent-detection']) {
    await cp(resolve(repoRoot, 'website', directory), resolve(publicDir, directory), {
      recursive: true,
    });
  }
}

async function prepareDocs({ draft }) {
  const manifest = JSON.parse(await readFile(versionsManifestPath, 'utf8'));
  if (
    manifest.schema_version !== 1 ||
    typeof manifest.current !== 'string' ||
    !['legacy', 'snapshot'].includes(manifest.stable_source ?? 'legacy')
  ) {
    throw new Error(`${versionsManifestPath} has an unsupported schema`);
  }
  const currentEntry = manifest.versions.find((entry) => entry.version === manifest.current);
  if (!currentEntry) throw new Error(`current docs version ${manifest.current} has no snapshot`);

  const previewManifest = JSON.parse(await readFile(previewManifestPath, 'utf8'));
  if (!/^[0-9a-f]{40}$/.test(previewManifest.commit ?? '')) {
    throw new Error(`${previewManifestPath} must contain a full preview commit SHA`);
  }
  const previewWebsiteRoot = resolve(repoRoot, draft ? 'docs/next/website' : 'docs/preview/website');
  const previewDocsSourceDir = resolve(previewWebsiteRoot, 'src/content/docs');
  const previewConfigReferenceSource = resolve(
    previewWebsiteRoot,
    'src/data/config-reference.json',
  );

  if ((manifest.stable_source ?? 'legacy') === 'snapshot') {
    await rm(stableDocsDir, { recursive: true, force: true });
    const currentSnapshotRoot = resolve(versionsDir, manifest.current, 'website');
    await copyPreparedDocs(
      resolve(currentSnapshotRoot, 'src/content/docs'),
      stableDocsDir,
      (content, relativePath) =>
        rewriteStableDocContent(content, {
          version: currentEntry.version,
          tag: currentEntry.tag,
          relativePath,
        }),
    );
    const currentReference = resolve(currentSnapshotRoot, 'src/data/config-reference.json');
    try {
      await cp(currentReference, stableConfigReferenceDestination);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      await rm(stableConfigReferenceDestination, { force: true });
    }
  } else {
    await rm(previewDocsDir, { recursive: true, force: true });
    await rm(generatedVersionsDocsDir, { recursive: true, force: true });
  }

  await copyPreparedDocs(previewDocsSourceDir, previewDocsDir, (content, relativePath) =>
    rewritePreviewDocContent(content, relativePath, {
      buildId: draft ? 'draft' : previewManifest.build_id,
      commit: draft ? 'master' : previewManifest.commit,
    }),
  );
  await cp(previewConfigReferenceSource, previewConfigReferenceDestination);

  const scopes = {
    stable: await collectDocsScope(
      (manifest.stable_source ?? 'legacy') === 'snapshot'
        ? resolve(versionsDir, manifest.current, 'website/src/content/docs')
        : stableDocsDir,
      new Set(['preview', '_versions']),
    ),
    preview: await collectDocsScope(previewDocsSourceDir),
  };
  const configReferences = {};

  for (const entry of manifest.versions) {
    const version = entry.version;
    const snapshotDocsRoot = resolve(versionsDir, version, 'website/src/content/docs');
    const destinationRoot = resolve(generatedVersionsDocsDir, version);
    await copyPreparedDocs(snapshotDocsRoot, destinationRoot, (content, relativePath) =>
      rewriteVersionDocContent(content, {
        version,
        tag: entry.tag,
        relativePath,
      }),
    );
    scopes[version] = await collectDocsScope(snapshotDocsRoot);

    const referencePath = resolve(
      versionsDir,
      version,
      'website/src/data/config-reference.json',
    );
    try {
      configReferences[version] = JSON.parse(await readFile(referencePath, 'utf8'));
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }

  await writeFile(
    generatedVersionsDataPath,
    `${JSON.stringify({
      ...manifest,
      preview: {
        build_id: draft ? 'draft' : previewManifest.build_id,
        commit: draft ? 'master' : previewManifest.commit,
      },
      scopes,
    }, null, 2)}\n`,
    'utf8',
  );
  await writeFile(
    generatedVersionReferencesPath,
    `${JSON.stringify(configReferences, null, 2)}\n`,
    'utf8',
  );
}

async function copyPreparedDocs(sourceDir, destinationDir, rewrite, pathPrefix = '') {
  await mkdir(destinationDir, { recursive: true });
  for (const entry of await readdir(sourceDir, { withFileTypes: true })) {
    const source = join(sourceDir, entry.name);
    const destination = join(destinationDir, entry.name);
    const relativePath = pathPrefix ? `${pathPrefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      await copyPreparedDocs(source, destination, rewrite, relativePath);
      continue;
    }
    if (!entry.isFile()) continue;

    if (!['.md', '.mdx'].includes(extname(entry.name).toLowerCase())) {
      await cp(source, destination);
      continue;
    }
    const content = await readFile(source, 'utf8');
    await writeFile(destination, rewrite(content, relativePath), 'utf8');
  }
}

async function collectDocsScope(sourceDir, excludedDirectories = new Set()) {
  const locales = { root: [], ja: [], 'zh-cn': [] };

  async function walk(directory, prefix = '') {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (entry.isDirectory() && excludedDirectories.has(entry.name)) continue;
      const path = join(directory, entry.name);
      const relativePath = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        await walk(path, relativePath);
        continue;
      }
      if (!entry.isFile() || !['.md', '.mdx'].includes(extname(entry.name))) continue;

      const segments = relativePath.replace(/\.(md|mdx)$/i, '').split('/');
      const locale = segments[0] === 'ja' || segments[0] === 'zh-cn' ? segments.shift() : 'root';
      const page = segments.join('/').replace(/(^|\/)index$/, '').replace(/\/$/, '');
      locales[locale].push(page);
    }
  }

  await walk(sourceDir);
  for (const pages of Object.values(locales)) pages.sort();
  for (const locale of ['ja', 'zh-cn']) {
    if (locales[locale].length === 0) locales[locale] = [...locales.root];
  }
  return { locales };
}

export function rewritePreviewDocContent(
  content,
  relativePath = '',
  { buildId, commit } = {
    buildId: 'preview',
    commit: 'master',
  },
) {
  const taggedContent = rewriteRepositoryLinks(
    content.replaceAll('/docs/', '/docs/preview/'),
    commit,
  );
  const rewritten = rewriteRelativeDocPaths(taggedContent, 1);
  const withSourceLink = setGeneratedEditUrl(
    rewritten,
    `https://github.com/herdrdev/herdr/blob/${commit}/docs/next/website/src/content/docs/${relativePath}`,
  );
  return insertPreviewNotice(withSourceLink, relativePath, { buildId, commit });
}

export function rewriteStableDocContent(content, { version, tag, relativePath }) {
  const taggedContent = rewriteRepositoryLinks(content, tag);
  return setGeneratedEditUrl(taggedContent, versionedDocSourceUrl(version, relativePath));
}

export function rewriteVersionDocContent(content, { version, tag, relativePath }) {
  const taggedContent = rewriteRepositoryLinks(
    content.replaceAll('/docs/', `/docs/${version}/`),
    tag,
  );
  const rewritten = rewriteRelativeDocPaths(taggedContent, 2);
  return setGeneratedEditUrl(rewritten, versionedDocSourceUrl(version, relativePath));
}

function versionedDocSourceUrl(version, relativePath) {
  return `https://github.com/herdrdev/herdr/blob/master/docs/versions/${version}/website/src/content/docs/${relativePath}`;
}

function rewriteRepositoryLinks(content, ref) {
  return content
    .replaceAll(
      'https://github.com/herdrdev/herdr/blob/master/',
      `https://github.com/herdrdev/herdr/blob/${ref}/`,
    )
    .replaceAll(
      'https://raw.githubusercontent.com/herdrdev/herdr/master/',
      `https://raw.githubusercontent.com/herdrdev/herdr/${ref}/`,
    );
}

function rewriteRelativeDocPaths(content, extraDepth) {
  const parents = '../'.repeat(extraDepth);
  return content
    .replace(/((?:\.\.\/)+)(?=public\/)/g, `$1${parents}`)
    .replace(/^(import .*from\s+['"])(?=(?:\.\.\/)+components\/)/gm, `$1${parents}`);
}

function setGeneratedEditUrl(content, editUrl) {
  if (!content.startsWith('---\n') || /^editUrl:/m.test(content)) return content;
  return content.replace(/^---\n/, `---\neditUrl: ${editUrl}\n`);
}

function insertPreviewNotice(content, relativePath, { buildId, commit }) {
  const source = commit === 'master' ? '`master` draft' : `[${commit.slice(0, 12)}](https://github.com/herdrdev/herdr/commit/${commit})`;
  const notice = [
    `> Preview build \`${buildId}\`, published from ${source}. Stable docs remain at [/docs/](/docs/).`,
    '',
    '',
  ].join('\n');
  const indexPrefix =
    relativePath === 'index.mdx'
      ? content.replace('title: Herdr documentation', 'title: Herdr preview documentation')
      : content;
  const frontmatter = indexPrefix.match(/^---\n[\s\S]*?\n---\n/);
  if (!frontmatter) {
    return insertNoticeAfterImports(indexPrefix, notice);
  }
  const body = indexPrefix.slice(frontmatter[0].length);
  return `${frontmatter[0]}\n${insertNoticeAfterImports(body, notice)}`;
}

function insertNoticeAfterImports(content, notice) {
  const imports = content.match(/^(\s*import .+?;\n)+\s*/);
  if (!imports) {
    return `${notice}${content}`;
  }
  return `${imports[0]}${notice}${content.slice(imports[0].length)}`;
}
