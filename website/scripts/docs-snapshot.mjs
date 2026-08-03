import { execFileSync } from 'node:child_process';
import { mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, relative, resolve } from 'node:path';

export function createGit(repoRoot) {
  return function git(args, options = {}) {
    return execFileSync('git', args, {
      cwd: repoRoot,
      encoding: options.binary ? undefined : 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  };
}

export function resolveCommit(git, ref) {
  return git(['rev-parse', `${ref}^{commit}`]).trim();
}

export function gitPathExists(git, ref, path) {
  try {
    git(['cat-file', '-e', `${ref}:${path}`]);
    return true;
  } catch {
    return false;
  }
}

export function gitTreesEqual(git, ref, left, right) {
  try {
    git(['diff', '--quiet', `${ref}:${left}`, `${ref}:${right}`]);
    return true;
  } catch {
    return false;
  }
}

export function listGitEntries(git, ref, root) {
  const output = git(['ls-tree', '-r', '-z', ref, '--', root]);
  return output
    .split('\0')
    .filter(Boolean)
    .map((line) => {
      const separator = line.indexOf('\t');
      const [mode, type, object] = line.slice(0, separator).split(' ');
      return { mode, type, object, path: line.slice(separator + 1) };
    });
}

export async function extractGitTree(git, ref, sourceRoot, destinationRoot) {
  const entries = listGitEntries(git, ref, sourceRoot);
  if (entries.length === 0) throw new Error(`${ref}:${sourceRoot} contains no files`);

  await rm(destinationRoot, { recursive: true, force: true });
  for (const entry of entries) {
    assertOrdinaryFile(entry, ref);
    const relativePath = relative(sourceRoot, entry.path);
    assertRelativePath(relativePath, entry.path);
    const destination = resolve(destinationRoot, relativePath);
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, git(['cat-file', 'blob', entry.object], { binary: true }));
  }
}

export async function compareGitTree(git, ref, sourceRoot, snapshotRoot) {
  const expectedEntries = listGitEntries(git, ref, sourceRoot);
  if (expectedEntries.length === 0) throw new Error(`${ref}:${sourceRoot} contains no files`);

  const expected = new Map();
  for (const entry of expectedEntries) {
    assertOrdinaryFile(entry, ref);
    const relativePath = relative(sourceRoot, entry.path);
    assertRelativePath(relativePath, entry.path);
    expected.set(relativePath, entry);
  }
  const actualPaths = await listSnapshotPaths(snapshotRoot);
  const expectedPaths = [...expected.keys()].sort();
  if (JSON.stringify(actualPaths) !== JSON.stringify(expectedPaths)) {
    throw new Error(`${snapshotRoot} file list differs from ${ref}:${sourceRoot}`);
  }

  for (const relativePath of expectedPaths) {
    const entry = expected.get(relativePath);
    const snapshotPath = resolve(snapshotRoot, relativePath);
    const expectedContent = git(['cat-file', 'blob', entry.object], { binary: true });
    if (!(await readFile(snapshotPath)).equals(expectedContent)) {
      throw new Error(`${relativePath} differs from ${ref}:${entry.path}`);
    }
  }
}

async function listSnapshotPaths(root) {
  const paths = [];
  async function walk(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.isFile()) paths.push(relative(root, path));
      else throw new Error(`${path} is not an ordinary documentation file`);
    }
  }
  await walk(root);
  return paths.sort();
}

function assertOrdinaryFile(entry, ref) {
  if (entry.type !== 'blob' || entry.mode !== '100644') {
    throw new Error(`${ref}:${entry.path} is not an ordinary documentation file`);
  }
}

function assertRelativePath(relativePath, originalPath) {
  if (!relativePath || relativePath === '..' || relativePath.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`)) {
    throw new Error(`unsafe snapshot path ${originalPath}`);
  }
}
