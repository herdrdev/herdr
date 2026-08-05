import { afterEach, describe, expect, test } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const script = resolve(import.meta.dir, 'docs-preview.mjs');
const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })),
  );
});

describe('preview documentation snapshots', () => {
  test('snapshots and validates the selected commit exactly', async () => {
    const root = await fixture();
    const commit = git(root, ['rev-parse', 'HEAD']).trim();
    await write(root, 'docs/preview/website/stale.mdx', 'stale\n');
    await writeManifest(root, commit);

    runScript(root, ['snapshot', commit]);
    runScript(root, ['check']);

    expect(await read(root, 'docs/preview/website/src/content/docs/index.mdx')).toBe(
      'selected preview\n',
    );
    await expect(read(root, 'docs/preview/website/stale.mdx')).rejects.toThrow();
  });

  test('rejects snapshot drift', async () => {
    const root = await fixture();
    const commit = git(root, ['rev-parse', 'HEAD']).trim();
    await writeManifest(root, commit);
    runScript(root, ['snapshot', commit]);
    await write(root, 'docs/preview/website/src/content/docs/index.mdx', 'changed\n');

    expect(() => runScript(root, ['check'])).toThrow();
  });
});

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'herdr-preview-docs-'));
  temporaryDirectories.push(root);
  await write(root, 'docs/next/website/src/content/docs/index.mdx', 'selected preview\n');
  await write(root, 'docs/next/website/src/data/config-reference.json', '{"preview":true}\n');
  git(root, ['init', '-q']);
  git(root, ['config', 'user.email', 'test@example.com']);
  git(root, ['config', 'user.name', 'Test']);
  git(root, ['add', '.']);
  git(root, ['commit', '-qm', 'preview fixture']);
  return root;
}

async function writeManifest(root: string, commit: string) {
  await write(
    root,
    'website/preview.json',
    `${JSON.stringify({
      schema_version: 1,
      channel: 'preview',
      build_id: 'test-build',
      commit,
      builds: { 'test-build': { commit, tag: 'preview-test-build' } },
    })}\n`,
  );
}

async function write(root: string, path: string, content: string) {
  const destination = resolve(root, path);
  await mkdir(resolve(destination, '..'), { recursive: true });
  await writeFile(destination, content, 'utf8');
}

async function read(root: string, path: string) {
  return readFile(resolve(root, path), 'utf8');
}

function git(root: string, args: string[]) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: 'pipe' });
}

function runScript(root: string, args: string[]) {
  execFileSync('node', [script, ...args], {
    cwd: root,
    env: { ...process.env, HERDR_DOCS_REPO_ROOT: root },
    stdio: 'pipe',
  });
}
