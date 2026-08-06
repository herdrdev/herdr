import { afterEach, describe, expect, test } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const script = resolve(import.meta.dir, 'docs-versions.mjs');
const prepareScript = resolve(import.meta.dir, 'prepare-docs.mjs');
const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe('documentation release publishing', () => {
  test('snapshots tagged next docs and switches stable to generated snapshots', async () => {
    const root = await mkdtemp(join(tmpdir(), 'herdr-docs-'));
    temporaryDirectories.push(root);
    await write(root, 'website/src/content/docs/index.mdx', 'stable docs\n');
    await write(root, 'website/src/data/config-reference.json', '{"stable":true}\n');
    await write(root, 'website/latest.json', '{"version":"0.9.0"}\n');
    await write(root, 'README.md', 'stable readme\n');
    await write(root, 'README.zh-CN.md', 'stable readme zh-cn\n');
    const nextDocs = '---\ntitle: Documentation\n---\n\nnext docs\n';
    await write(root, 'docs/next/website/src/content/docs/index.mdx', nextDocs);
    await write(root, 'docs/next/website/src/data/config-reference.json', '{"next":true}\n');
    await write(root, 'docs/next/README.md', 'next readme\n');
    await write(root, 'docs/next/README.zh-CN.md', 'next readme zh-cn\n');

    git(root, ['init', '-q']);
    git(root, ['config', 'user.email', 'test@example.com']);
    git(root, ['config', 'user.name', 'Test']);
    git(root, ['add', '.']);
    git(root, ['commit', '-qm', 'release fixture']);
    git(root, ['tag', 'v0.9.0']);
    git(root, ['tag', 'v1.0.0']);

    runScript(root, ['publish', 'v1.0.0']);

    await expect(read(root, 'website/src/content/docs/index.mdx')).rejects.toThrow();
    await expect(read(root, 'website/src/data/config-reference.json')).rejects.toThrow();
    expect(await read(root, 'README.md')).toBe('next readme\n');
    expect(await read(root, 'README.zh-CN.md')).toBe('next readme zh-cn\n');
    expect(await read(root, 'docs/versions/1.0.0/website/src/content/docs/index.mdx')).toBe(nextDocs);

    const manifest = JSON.parse(await read(root, 'docs/versions/manifest.json'));
    expect(manifest.current).toBe('1.0.0');
    expect(manifest.stable_source).toBe('snapshot');
    expect(manifest.versions[0]).toMatchObject({
      version: '1.0.0',
      tag: 'v1.0.0',
      commit: expect.stringMatching(/^[0-9a-f]{40}$/),
      source: 'docs/next/website/src/content/docs',
    });

    const previewCommit = git(root, ['rev-parse', 'HEAD']).trim();
    await write(root, 'docs/preview/website/src/content/docs/index.mdx', 'preview docs\n');
    await write(
      root,
      'docs/preview/website/src/data/config-reference.json',
      '{"preview":true}\n',
    );
    await write(
      root,
      'website/preview.json',
      `${JSON.stringify({ build_id: 'preview-test', commit: previewCommit })}\n`,
    );
    runPrepare(root);
    const stableDocs = await read(root, 'website/src/content/docs/index.mdx');
    expect(stableDocs).toContain('next docs');
    expect(stableDocs).toContain(
      'editUrl: https://github.com/herdrdev/herdr/blob/master/docs/versions/1.0.0/website/src/content/docs/index.mdx',
    );
    const versionedDocs = await read(
      root,
      'website/src/content/docs/_versions/1.0.0/index.mdx',
    );
    expect(versionedDocs).toContain('next docs');
    expect(versionedDocs).toContain(
      'editUrl: https://github.com/herdrdev/herdr/blob/master/docs/versions/1.0.0/website/src/content/docs/index.mdx',
    );
    expect(await read(root, 'website/src/content/docs/preview/index.mdx')).toContain(
      'Preview build `preview-test`',
    );
    expect(await read(root, 'website/src/data/config-reference.json')).toBe('{"next":true}\n');

    await write(root, 'website/latest.json', '{"version":"1.0.0"}\n');
    runScript(root, ['check']);

    const correctedDocs = '---\ntitle: Documentation\n---\n\ncorrected docs\n';
    await write(
      root,
      'docs/versions/1.0.0/website/src/content/docs/index.mdx',
      correctedDocs,
    );
    await write(
      root,
      'docs/versions/1.0.0/website/src/data/config-reference.json',
      '{"corrected":true}\n',
    );
    runScript(root, ['check']);

    await write(root, 'README.md', 'post-release correction\n');
    runScript(root, ['publish', 'v1.0.0']);
    expect(await read(root, 'README.md')).toBe('post-release correction\n');

    runPrepare(root);
    expect(await read(root, 'website/src/content/docs/index.mdx')).toContain('corrected docs');
    expect(await read(root, 'website/src/content/docs/_versions/1.0.0/index.mdx')).toContain(
      'corrected docs',
    );
    expect(await read(root, 'website/src/data/config-reference.json')).toBe(
      '{"corrected":true}\n',
    );
    const versionReferences = JSON.parse(
      await read(root, 'website/src/data/config-reference-versions.json'),
    );
    expect(versionReferences['1.0.0']).toEqual({ corrected: true });

    runScript(root, ['publish', 'v0.9.0']);
    const archivedManifest = JSON.parse(await read(root, 'docs/versions/manifest.json'));
    expect(archivedManifest.current).toBe('1.0.0');
    expect(archivedManifest.versions.map(({ version }) => version)).toEqual(['1.0.0', '0.9.0']);
    expect(await read(root, 'README.md')).toBe('post-release correction\n');
    runScript(root, ['check']);

    const correctedArchivedDocs = '---\ntitle: Documentation\n---\n\ncorrected archived docs\n';
    await write(
      root,
      'docs/versions/0.9.0/website/src/content/docs/index.mdx',
      correctedArchivedDocs,
    );
    runScript(root, ['publish', 'v0.9.0']);
    expect(await read(root, 'docs/versions/0.9.0/website/src/content/docs/index.mdx')).toBe(
      correctedArchivedDocs,
    );

    const archivedDocsRoot = resolve(
      root,
      'docs/versions/0.9.0/website/src/content/docs',
    );
    await rm(archivedDocsRoot, { recursive: true, force: true });
    expect(() => runScript(root, ['check'])).toThrow();
    await write(
      root,
      'docs/versions/0.9.0/website/src/content/docs/index.mdx',
      correctedArchivedDocs,
    );
    const linkedPage = resolve(archivedDocsRoot, 'linked.mdx');
    await symlink('index.mdx', linkedPage);
    expect(() => runScript(root, ['check'])).toThrow();
    await rm(linkedPage);
    runScript(root, ['check']);

    delete archivedManifest.versions[0].commit;
    await write(root, 'docs/versions/manifest.json', `${JSON.stringify(archivedManifest)}\n`);
    expect(() => runScript(root, ['check'])).toThrow();
  });
});

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

function runPrepare(root: string) {
  execFileSync('node', [prepareScript, '--docs-only'], {
    cwd: root,
    env: { ...process.env, HERDR_DOCS_REPO_ROOT: root },
    stdio: 'pipe',
  });
}
