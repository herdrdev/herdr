import { readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  compareGitTree,
  createGit,
  extractGitTree,
  gitPathExists,
  resolveCommit,
} from './docs-snapshot.mjs';

const websiteDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = process.env.HERDR_DOCS_REPO_ROOT
  ? resolve(process.env.HERDR_DOCS_REPO_ROOT)
  : resolve(websiteDir, '../..');
const git = createGit(repoRoot);
const sourceRoot = 'docs/next/website';
const snapshotRoot = resolve(repoRoot, 'docs/preview/website');
const manifestPath = resolve(repoRoot, 'website/preview.json');
const fullShaPattern = /^[0-9a-f]{40}$/;

export async function snapshotPreview(ref) {
  const commit = resolveCommit(git, ref);
  if (!gitPathExists(git, commit, `${sourceRoot}/src/content/docs`)) {
    throw new Error(`${commit} does not contain staged website documentation`);
  }
  if (!gitPathExists(git, commit, `${sourceRoot}/src/data/config-reference.json`)) {
    throw new Error(`${commit} does not contain the staged config reference`);
  }
  await extractGitTree(git, commit, sourceRoot, snapshotRoot);
  process.stdout.write(`snapshotted preview documentation from ${commit}\n`);
}

export async function checkPreview() {
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (manifest.schema_version !== 1 || manifest.channel !== 'preview') {
    throw new Error(`${manifestPath} has an unsupported schema`);
  }
  if (typeof manifest.commit !== 'string' || !fullShaPattern.test(manifest.commit)) {
    throw new Error(`${manifestPath} must contain a full preview commit SHA`);
  }
  const build = manifest.builds?.[manifest.build_id];
  if (!build || build.commit !== manifest.commit) {
    throw new Error(`${manifestPath} active build does not match its preview commit`);
  }
  const expectedTag = `preview-${manifest.build_id}`;
  if (build.tag !== expectedTag) {
    throw new Error(`${manifestPath} active build tag does not match ${expectedTag}`);
  }
  if (!gitPathExists(git, manifest.commit, `${sourceRoot}/src/content/docs`)) {
    throw new Error(`${manifest.commit} does not contain staged website documentation`);
  }
  if (!gitPathExists(git, manifest.commit, `${sourceRoot}/src/data/config-reference.json`)) {
    throw new Error(`${manifest.commit} does not contain the staged config reference`);
  }
  await compareGitTree(git, manifest.commit, sourceRoot, snapshotRoot);
  process.stdout.write(`validated preview documentation snapshot ${manifest.commit}\n`);
}

async function main() {
  const [command, value] = process.argv.slice(2);
  if (command === 'snapshot' && value) {
    await snapshotPreview(value);
    return;
  }
  if (command === 'check' && !value) {
    await checkPreview();
    return;
  }
  throw new Error('usage: node website/scripts/docs-preview.mjs check | snapshot <commit>');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
