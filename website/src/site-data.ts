import { readdir, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

/* The numbers the site's chrome shows, resolved once per build.
   Stars, installs and plugin counts come from the same R2 snapshots /stats/
   and /plugins/ read; the version comes from latest.json, which release CI
   owns. Nothing here is hand-maintained, so no figure on the site can go
   stale independently of a deploy.

   Memoised because SiteLayout renders on every blog post, and a build that
   renders many pages should still fetch once. */

export type SiteStats = {
  starCount?: number;
  installCount?: number;
  pluginCount?: number;
};

async function snapshot(url: string): Promise<any | null> {
  try {
    const res = await fetch(url);
    return res.ok ? await res.json() : null;
  } catch {
    return null;
  }
}

async function load(): Promise<SiteStats> {
  const [stats, plugins] = await Promise.all([
    snapshot('https://assets.herdr.dev/stats/stats.json'),
    snapshot('https://assets.herdr.dev/plugins/index.json'),
  ]);

  return {
    starCount: stats?.stars?.total,
    installCount: stats?.downloads
      ? (stats.downloads.github || 0) + (stats.downloads.brew || 0)
      : undefined,
    /* `pluginCount` counts validated manifests, which is what the marketplace
       lists. `source.totalCount` is the raw GitHub topic search and still
       includes blacklisted repositories and ones with no manifest. */
    pluginCount: plugins?.pluginCount ?? plugins?.plugins?.length,
  };
}

let statsPromise: Promise<SiteStats> | null = null;

export function siteStats(): Promise<SiteStats> {
  statsPromise ??= load();
  return statsPromise;
}

/* latest.json carries the full release notes, so read and pick rather than
   importing it and bundling all of that into the page. */
async function readVersion(): Promise<string | null> {
  try {
    const raw = await readFile(new URL('../latest.json', import.meta.url), 'utf8');
    return JSON.parse(raw).version ?? null;
  } catch {
    return null;
  }
}

let versionPromise: Promise<string | null> | null = null;

export function releaseVersion(): Promise<string | null> {
  versionPromise ??= readVersion();
  return versionPromise;
}

/* How many agents Herdr detects out of the box, counted from the bundled
   manifests. The site is built from the same repo as the binary, so the number
   tracks the code and cannot drift the way a written-down one would — add a
   manifest and the homepage says so on the next deploy. */
async function countAgentManifests(): Promise<number | null> {
  /* Resolved from cwd, not import.meta.url: once Vite bundles this module for
     the build the module URL no longer points at the source file, so a
     relative URL silently found nothing and the line vanished from dist while
     still rendering in dev. Both candidates cover running the build from
     website/ or from the repo root. */
  for (const rel of ['../src/detect/manifests', 'src/detect/manifests']) {
    try {
      const files = await readdir(resolve(process.cwd(), rel));
      const count = files.filter((f) => f.endsWith('.toml')).length;
      if (count > 0) return count;
    } catch {
      /* try the next candidate */
    }
  }
  return null;
}

let agentCountPromise: Promise<number | null> | null = null;

export function agentCount(): Promise<number | null> {
  agentCountPromise ??= countAgentManifests();
  return agentCountPromise;
}

/* A fetch that failed at build shows an em dash. A stale hardcoded number
   would look authoritative and be wrong, which is worse. */
export const fmt = (n: unknown): string =>
  typeof n === 'number' ? n.toLocaleString('en-US') : '—';
