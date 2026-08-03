import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  compareGitTree,
  createGit,
  extractGitTree,
  gitPathExists,
  gitTreesEqual,
  resolveCommit,
} from './docs-snapshot.mjs';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = process.env.HERDR_DOCS_REPO_ROOT
  ? resolve(process.env.HERDR_DOCS_REPO_ROOT)
  : resolve(websiteDir, '../..');
const versionsDir = resolve(repoRoot, 'docs/versions');
const manifestPath = resolve(versionsDir, 'manifest.json');
const stableDocsDir = resolve(repoRoot, 'website/src/content/docs');
const stableReferencePath = resolve(repoRoot, 'website/src/data/config-reference.json');
const git = createGit(repoRoot);

const VERSION_PATTERN = /^v?(\d+\.\d+\.\d+)$/;

export function normalizeVersion(value) {
  const match = VERSION_PATTERN.exec(value);
  if (!match) throw new Error(`version must look like 0.7.5 or v0.7.5, got ${value}`);
  return match[1];
}

export function sortVersionsNewestFirst(versions) {
  return [...versions].sort((left, right) => compareVersions(right.version, left.version));
}

function compareVersions(left, right) {
  const a = left.split('.').map(Number);
  const b = right.split('.').map(Number);
  for (let index = 0; index < 3; index += 1) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return 0;
}

async function readManifest() {
  try {
    return JSON.parse(await readFile(manifestPath, 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') {
      return { schema_version: 1, stable_source: 'legacy', current: null, versions: [] };
    }
    throw error;
  }
}

async function writeManifest(manifest) {
  manifest.versions = sortVersionsNewestFirst(manifest.versions).map(
    ({ version, tag, commit, source }) => ({
      version,
      tag,
      ...(commit ? { commit } : {}),
      source,
    }),
  );
  await mkdir(dirname(manifestPath), { recursive: true });
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
}

function releaseMetadata(tag) {
  const version = normalizeVersion(tag);
  const normalizedTag = `v${version}`;
  return { version, tag: normalizedTag, commit: resolveCommit(git, normalizedTag) };
}

async function snapshotTag(tag, sourceRoot) {
  const metadata = { ...releaseMetadata(tag), source: sourceRoot };
  const destination = resolve(versionsDir, metadata.version);
  await rm(destination, { recursive: true, force: true });
  await extractGitTree(git, tag, sourceRoot, resolve(destination, 'website/src/content/docs'));

  const referenceSource = sourceRoot.includes('docs/next/')
    ? 'docs/next/website/src/data/config-reference.json'
    : 'website/src/data/config-reference.json';
  if (gitPathExists(git, tag, referenceSource)) {
    const referenceDestination = resolve(destination, 'website/src/data/config-reference.json');
    await mkdir(dirname(referenceDestination), { recursive: true });
    await writeFile(referenceDestination, git(['show', `${tag}:${referenceSource}`], { binary: true }));
  }

  return metadata;
}

export async function backfillVersions() {
  const tags = git(['tag', '--list', 'v*', '--sort=v:refname']).split('\n').filter(Boolean);
  const manifest = await readManifest();
  const entries = new Map(manifest.versions.map((entry) => [entry.version, entry]));

  for (const tag of tags) {
    const version = normalizeVersion(tag);
    if (entries.has(version)) continue;
    const nextRoot = 'docs/next/website/src/content/docs';
    const legacyRoot = 'website/src/content/docs';
    const hasNext = gitPathExists(git, tag, nextRoot);
    const hasLegacy = gitPathExists(git, tag, legacyRoot);
    if (!hasNext && !hasLegacy) continue;
    const sourceRoot =
      hasNext && (!hasLegacy || !gitTreesEqual(git, tag, legacyRoot, nextRoot))
        ? nextRoot
        : legacyRoot;
    const metadata = await snapshotTag(tag, sourceRoot);
    entries.set(metadata.version, metadata);
    process.stdout.write(`snapshotted ${tag}\n`);
  }

  const current = JSON.parse(await readFile(resolve(repoRoot, 'website/latest.json'), 'utf8')).version;
  await writeManifest({
    schema_version: 1,
    stable_source: manifest.stable_source ?? 'legacy',
    current: normalizeVersion(current),
    versions: [...entries.values()],
  });
}

export async function checkVersions() {
  const manifest = await readManifest();
  if (
    manifest.schema_version !== 1 ||
    typeof manifest.current !== 'string' ||
    !['legacy', 'snapshot'].includes(manifest.stable_source ?? 'legacy')
  ) {
    throw new Error(`${manifestPath} has an unsupported schema`);
  }

  const latest = JSON.parse(await readFile(resolve(repoRoot, 'website/latest.json'), 'utf8')).version;
  if (manifest.current !== normalizeVersion(latest)) {
    throw new Error(`docs current version ${manifest.current} does not match website latest ${latest}`);
  }

  const expectedOrder = sortVersionsNewestFirst(manifest.versions).map(({ version }) => version);
  if (JSON.stringify(manifest.versions.map(({ version }) => version)) !== JSON.stringify(expectedOrder)) {
    throw new Error('documentation versions are not sorted newest first');
  }

  const seen = new Set();
  for (const entry of manifest.versions) {
    if (seen.has(entry.version)) throw new Error(`duplicate docs version ${entry.version}`);
    seen.add(entry.version);
    if (normalizeVersion(entry.tag) !== entry.version) {
      throw new Error(`docs version ${entry.version} has mismatched tag ${entry.tag}`);
    }
    const requiresCommit =
      entry.source === 'docs/next/website/src/content/docs' ||
      ((manifest.stable_source ?? 'legacy') === 'snapshot' && entry.version === manifest.current);
    if (requiresCommit && !entry.commit) {
      throw new Error(`docs version ${entry.version} is missing commit provenance`);
    }
    if (entry.commit) {
      if (!/^[0-9a-f]{40}$/.test(entry.commit)) {
        throw new Error(`docs version ${entry.version} has invalid commit ${entry.commit}`);
      }
      const taggedCommit = resolveCommit(git, entry.tag);
      if (taggedCommit !== entry.commit) {
        throw new Error(
          `docs version ${entry.version} tag ${entry.tag} moved from ${entry.commit} to ${taggedCommit}`,
        );
      }
    }
    if (!['website/src/content/docs', 'docs/next/website/src/content/docs'].includes(entry.source)) {
      throw new Error(`docs version ${entry.version} has unsupported source ${entry.source}`);
    }
    const sourceRoot = entry.source;
    const snapshotRoot = resolve(versionsDir, entry.version, 'website/src/content/docs');
    await compareGitTree(git, entry.tag, sourceRoot, snapshotRoot);

    const referenceSource = sourceRoot.includes('docs/next/')
      ? 'docs/next/website/src/data/config-reference.json'
      : 'website/src/data/config-reference.json';
    const referenceSnapshot = resolve(
      versionsDir,
      entry.version,
      'website/src/data/config-reference.json',
    );
    if (gitPathExists(git, entry.tag, referenceSource)) {
      const actualReference = await readFile(referenceSnapshot);
      const taggedReference = git(['show', `${entry.tag}:${referenceSource}`], { binary: true });
      if (!actualReference.equals(taggedReference)) {
        throw new Error(`${entry.version} config reference differs from ${entry.tag}:${referenceSource}`);
      }
    } else {
      try {
        await readFile(referenceSnapshot);
        throw new Error(`${entry.version} has an unexpected config reference snapshot`);
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
      }
    }
  }

  if (!seen.has(manifest.current)) {
    throw new Error(`current docs version ${manifest.current} has no snapshot`);
  }
  process.stdout.write(`validated ${manifest.versions.length} documentation snapshots\n`);
}

export async function publishVersion(tag) {
  const release = releaseMetadata(tag);
  if (!gitPathExists(git, tag, 'docs/next/website/src/content/docs')) {
    throw new Error(`${tag} does not contain staged website documentation`);
  }

  const manifest = await readManifest();
  const existing = manifest.versions.find((entry) => entry.version === release.version);
  if (manifest.current === release.version) {
    if ((manifest.stable_source ?? 'legacy') === 'legacy') {
      process.stdout.write(`documentation ${release.tag} is already published with the legacy stable source\n`);
      return;
    }
    if (!existing?.commit || existing.commit !== release.commit) {
      throw new Error(`published documentation ${release.version} has mismatched commit provenance`);
    }
    await compareGitTree(git, release.commit, existing.source, resolve(versionsDir, release.version, 'website/src/content/docs'));
    process.stdout.write(`documentation snapshot ${release.tag} is already published\n`);
    return;
  }
  if (manifest.current && compareVersions(release.version, manifest.current) < 0) {
    if (existing) {
      if (existing.tag !== release.tag) {
        throw new Error(`version ${release.version} is already associated with ${existing.tag}`);
      }
      if (existing.commit && existing.commit !== release.commit) {
        throw new Error(`version ${release.version} has mismatched commit provenance`);
      }
      await compareGitTree(
        git,
        release.commit,
        existing.source,
        resolve(versionsDir, release.version, 'website/src/content/docs'),
      );
      process.stdout.write(`documentation snapshot ${release.tag} is already archived\n`);
      return;
    }
    const archived = await snapshotTag(tag, 'docs/next/website/src/content/docs');
    const archivedEntries = new Map(manifest.versions.map((entry) => [entry.version, entry]));
    archivedEntries.set(archived.version, archived);
    await writeManifest({
      ...manifest,
      versions: [...archivedEntries.values()],
    });
    process.stdout.write(
      `archived documentation snapshot ${release.tag} without changing current ${manifest.current}\n`,
    );
    return;
  }
  if (existing) {
    throw new Error(`version ${release.version} already has a documentation snapshot`);
  }

  const metadata = await snapshotTag(tag, 'docs/next/website/src/content/docs');
  await rm(stableDocsDir, { recursive: true, force: true });
  await rm(stableReferencePath, { force: true });

  for (const readme of ['README.md', 'README.zh-CN.md']) {
    const nextReadme = `docs/next/${readme}`;
    if (gitPathExists(git, tag, nextReadme)) {
      await writeFile(resolve(repoRoot, readme), git(['show', `${tag}:${nextReadme}`], { binary: true }));
    }
  }

  const entries = new Map(manifest.versions.map((entry) => [entry.version, entry]));
  entries.set(metadata.version, metadata);
  await writeManifest({
    schema_version: 1,
    stable_source: 'snapshot',
    current: metadata.version,
    versions: [...entries.values()],
  });
  process.stdout.write(`published documentation snapshot ${metadata.tag}\n`);
}

async function main() {
  const [command, value] = process.argv.slice(2);
  if (command === 'backfill' && !value) {
    await backfillVersions();
    return;
  }
  if (command === 'publish' && value) {
    await publishVersion(value);
    return;
  }
  if (command === 'check' && !value) {
    await checkVersions();
    return;
  }
  if (command === 'current' && !value) {
    const manifest = await readManifest();
    process.stdout.write(`${manifest.current ?? ''}\n`);
    return;
  }
  throw new Error(
    'usage: node website/scripts/docs-versions.mjs backfill | check | current | publish <tag>',
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
