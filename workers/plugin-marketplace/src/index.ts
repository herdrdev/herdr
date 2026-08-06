import { parse } from "smol-toml";

const SNAPSHOT_KEY = "plugins/index.json";
const SCAN_CACHE_KEY = "plugins/scan-cache.json";
const SNAPSHOT_CACHE_CONTROL = "public, max-age=300, s-maxage=1800, stale-while-revalidate=3600";
const GITHUB_QUERY = "topic:herdr-plugin is:public";
const GITHUB_API_VERSION = "2022-11-28";
const GITHUB_API_URL = "https://api.github.com";
const GITHUB_SEARCH_URL = `${GITHUB_API_URL}/search/repositories`;
const GITHUB_GRAPHQL_URL = `${GITHUB_API_URL}/graphql`;
const BLACKLIST_REPO_KEY_PREFIX = "repo:";
const PLUGIN_MANIFEST_FILE = "herdr-plugin.toml";
const PER_PAGE = 100;
const MAX_REPOS = 1000;
const GRAPHQL_BATCH_SIZE = 50;
const NETWORK_CONCURRENCY = 5;
const MANIFEST_MAX_BYTES = 32 * 1024;
const MAX_TREE_ENTRIES = 20_000;
const MAX_MANIFESTS_PER_REPOSITORY = 100;
const MAX_TOTAL_MANIFESTS = 5000;
const MAX_TOTAL_MANIFEST_BYTES = 16 * 1024 * 1024;
const PLUGIN_NAME_MAX_CHARS = 120;
const PLUGIN_VERSION_MAX_CHARS = 64;
const PLUGIN_DESCRIPTION_MAX_CHARS = 500;
const REQUEST_TIMEOUT_MS = 10_000;
const SCAN_CACHE_SCHEMA_VERSION = 1;

const PLUGIN_PLATFORMS = new Set(["linux", "macos", "windows"]);
const REGULAR_BLOB_MODES = new Set(["100644", "100755"]);

type R2Bucket = {
  put(
    key: string,
    value: string,
    options?: {
      httpMetadata?: {
        contentType?: string;
        cacheControl?: string;
      };
    },
  ): Promise<unknown>;
  get(key: string): Promise<{ text(): Promise<string> } | null>;
};

type KVNamespace = {
  list(options?: { prefix?: string; cursor?: string }): Promise<{
    keys: Array<{ name: string }>;
    cursor?: string;
  }>;
};

type ExecutionContext = {
  waitUntil(promise: Promise<unknown>): void;
};

type ScheduledController = unknown;

export type Env = {
  PLUGIN_MARKETPLACE_BUCKET: R2Bucket;
  PLUGIN_MARKETPLACE_BLACKLIST?: KVNamespace;
  GITHUB_TOKEN?: string;
};

type FetchLike = typeof fetch;

type RefreshOptions = {
  fetch?: FetchLike;
  now?: Date;
  logger?: Pick<Console, "error">;
};

type GitHubRepository = Record<string, unknown>;

type RepositoryListing = {
  id: number;
  fullName: string;
  owner: string;
  name: string;
  description: string | null;
  url: string;
  stars: number;
  forks: number;
  openIssues: number;
  language: string | null;
  topics: string[];
  createdAt: string | null;
  updatedAt: string | null;
  pushedAt: string | null;
  defaultBranch: string;
};

export type PluginManifestListing = {
  path: string;
  id: string;
  name: string;
  version: string;
  minHerdrVersion: string;
  description: string | null;
  platforms: string[] | null;
};

export type PluginListing = Omit<RepositoryListing, "defaultBranch"> & {
  headCommit: string;
  manifests: PluginManifestListing[];
};

export type PluginSnapshot = {
  schemaVersion: 1;
  generatedAt: string;
  pluginCount: number;
  repositoryCount: number;
  source: {
    provider: "github";
    query: string;
    totalCount: number;
    collectedCount: number;
    blacklistedCount: number;
    missingManifestCount: number;
    invalidManifestCount: number;
    duplicateManifestCount: number;
    skippedRepositoryCount: number;
    unavailableRepositoryCount: number;
    truncated: boolean;
    warnings?: string[];
  };
  plugins: PluginListing[];
};

type ScanCacheEntry = {
  repositoryId: number;
  fullName: string;
  defaultBranch: string;
  pushedAt: string | null;
  headCommit: string;
  manifestFileCount: number;
  invalidManifestCount: number;
  duplicateManifestCount: number;
  warning: string | null;
  retryable: boolean;
  manifests: PluginManifestListing[];
};

type ScanCache = {
  schemaVersion: 1;
  entries: ScanCacheEntry[];
};

type ResolvedRepository = {
  repository: RepositoryListing;
  headCommit: string;
  cached: ScanCacheEntry | null;
};

type TreeManifestCandidate = {
  repository: RepositoryListing;
  headCommit: string;
  path: string;
  size: number;
};

type PendingRepositoryScan = {
  resolved: ResolvedRepository;
  manifestFileCount: number;
  invalidManifestCount: number;
  warning: string | null;
  retryable: boolean;
  candidates: TreeManifestCandidate[];
};

type GitHubTreeEntry = {
  path: string;
  mode: string;
  type: string;
  size: number | null;
};

export type RefreshResult =
  | { ok: true; snapshot: PluginSnapshot }
  | { ok: false; error: string };

export default {
  fetch(): Response {
    return jsonResponse({ error: "Not found" }, 404, "no-store");
  },

  scheduled(_event: ScheduledController, env: Env, ctx: ExecutionContext): void {
    ctx.waitUntil(refreshPlugins(env));
  },
};

export async function refreshPlugins(
  env: Env,
  options: RefreshOptions = {},
): Promise<RefreshResult> {
  const logger = options.logger ?? console;
  try {
    const token = env.GITHUB_TOKEN?.trim();
    if (!token) {
      throw new Error("GITHUB_TOKEN is not configured");
    }

    const fetchFn = options.fetch ?? fetch;
    const search = await fetchGitHubRepositories(fetchFn, token);
    const repositories = normalizeRepositories(search.repositories);
    const blockedRepositories = await readBlacklistedRepositories(env);
    const candidates = repositories.filter(
      (repository) => !blockedRepositories.has(repository.fullName.toLowerCase()),
    );
    const blacklistedCount = repositories.length - candidates.length;
    const cache = await readScanCache(env.PLUGIN_MARKETPLACE_BUCKET, logger);
    if (cache.entries.length > 0 && repositories.length * 2 < cache.entries.length) {
      throw new Error(
        `GitHub repository inventory collapsed from ${cache.entries.length} to ${repositories.length}; remove ${SCAN_CACHE_KEY} to accept an intentional reset`,
      );
    }
    const cachedById = new Map(cache.entries.map((entry) => [entry.repositoryId, entry]));

    const { resolved, unavailableRepositoryCount } = await resolveRepositoryHeads(
      fetchFn,
      token,
      candidates,
      cachedById,
    );
    const resolvedIds = new Set(resolved.map(({ repository }) => repository.id));
    const missingCachedRepository = candidates.find(
      (repository) => cachedById.has(repository.id) && !resolvedIds.has(repository.id),
    );
    if (
      missingCachedRepository ||
      (candidates.length > 0 && resolved.length * 2 < candidates.length)
    ) {
      throw new Error(
        `GitHub head resolution omitted marketplace repositories${missingCachedRepository ? ` including ${missingCachedRepository.fullName}` : ""}`,
      );
    }
    const changed = resolved.filter(
      ({ headCommit, cached }) => !cached || cached.headCommit !== headCommit || cached.retryable,
    );
    const scannedById = await scanChangedRepositories(fetchFn, token, changed);

    const nextEntries = resolved.map(({ repository, headCommit, cached }) => {
      const scanned = scannedById.get(repository.id);
      if (scanned) return scanned;
      if (!cached || cached.headCommit !== headCommit) {
        throw new Error(`missing marketplace scan result for ${repository.fullName}`);
      }
      return {
        ...cached,
        fullName: repository.fullName,
        defaultBranch: repository.defaultBranch,
        pushedAt: repository.pushedAt,
      };
    });
    const entriesById = new Map(nextEntries.map((entry) => [entry.repositoryId, entry]));
    const plugins = resolved
      .map(({ repository }) => {
        const entry = entriesById.get(repository.id);
        if (!entry || entry.manifests.length === 0) return null;
        const { defaultBranch: _defaultBranch, ...card } = repository;
        return { ...card, headCommit: entry.headCommit, manifests: entry.manifests };
      })
      .filter((plugin): plugin is PluginListing => plugin !== null);

    const pluginCount = plugins.reduce((total, plugin) => total + plugin.manifests.length, 0);
    const warnings = nextEntries.flatMap((entry) => (entry.warning ? [entry.warning] : []));
    if (search.truncated) {
      warnings.unshift(
        `GitHub returned ${search.totalCount} results; only the first ${search.repositories.length} were collected.`,
      );
    }
    const generatedAt = (options.now ?? new Date()).toISOString();
    const snapshot: PluginSnapshot = {
      schemaVersion: 1,
      generatedAt,
      pluginCount,
      repositoryCount: plugins.length,
      source: {
        provider: "github",
        query: GITHUB_QUERY,
        totalCount: search.totalCount,
        collectedCount: search.repositories.length,
        blacklistedCount,
        missingManifestCount: nextEntries.filter(
          (entry) => entry.manifestFileCount === 0 && !entry.warning,
        ).length,
        invalidManifestCount: nextEntries.reduce(
          (total, entry) => total + entry.invalidManifestCount,
          0,
        ),
        duplicateManifestCount: nextEntries.reduce(
          (total, entry) => total + entry.duplicateManifestCount,
          0,
        ),
        skippedRepositoryCount: nextEntries.filter((entry) => entry.warning).length,
        unavailableRepositoryCount,
        truncated: search.truncated,
        ...(warnings.length > 0 ? { warnings } : {}),
      },
      plugins,
    };

    const nextCache: ScanCache = { schemaVersion: SCAN_CACHE_SCHEMA_VERSION, entries: nextEntries };
    await env.PLUGIN_MARKETPLACE_BUCKET.put(SCAN_CACHE_KEY, JSON.stringify(nextCache), {
      httpMetadata: {
        contentType: "application/json; charset=utf-8",
        cacheControl: "no-store",
      },
    });
    await env.PLUGIN_MARKETPLACE_BUCKET.put(SNAPSHOT_KEY, JSON.stringify(snapshot), {
      httpMetadata: {
        contentType: "application/json; charset=utf-8",
        cacheControl: SNAPSHOT_CACHE_CONTROL,
      },
    });
    return { ok: true, snapshot };
  } catch (error) {
    const message = error instanceof Error ? error.message : "unknown refresh error";
    logger.error(`plugin marketplace refresh failed: ${message}`);
    return { ok: false, error: message };
  }
}

async function fetchGitHubRepositories(
  fetchFn: FetchLike,
  token: string,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<{ repositories: GitHubRepository[]; totalCount: number; truncated: boolean }> {
  const repositories: GitHubRepository[] = [];
  let totalCount = 0;

  for (let page = 1; repositories.length < MAX_REPOS; page += 1) {
    const url = new URL(GITHUB_SEARCH_URL);
    url.searchParams.set("q", GITHUB_QUERY);
    url.searchParams.set("per_page", String(PER_PAGE));
    url.searchParams.set("page", String(page));
    url.searchParams.set("sort", "stars");
    url.searchParams.set("order", "desc");

    const response = await fetchWithTimeout(fetchFn, url, githubRequestInit(token), timeoutMs);
    if (!response.ok) {
      throw new Error(`GitHub search failed with status ${response.status}`);
    }
    const body = await response.json();
    if (!isObject(body) || typeof body.total_count !== "number" || !Array.isArray(body.items)) {
      throw new Error("GitHub search returned malformed JSON");
    }
    if (body.incomplete_results === true) {
      throw new Error("GitHub search returned incomplete results");
    }

    totalCount = body.total_count;
    repositories.push(...body.items.slice(0, MAX_REPOS - repositories.length));
    if (repositories.length >= totalCount || body.items.length === 0) break;
  }

  return {
    repositories,
    totalCount,
    truncated: totalCount > repositories.length,
  };
}

async function resolveRepositoryHeads(
  fetchFn: FetchLike,
  token: string,
  repositories: RepositoryListing[],
  cachedById: Map<number, ScanCacheEntry>,
): Promise<{ resolved: ResolvedRepository[]; unavailableRepositoryCount: number }> {
  const resolvedById = new Map<number, ResolvedRepository>();
  const needResolution: RepositoryListing[] = [];

  for (const repository of repositories) {
    const cached = cachedById.get(repository.id) ?? null;
    if (
      cached &&
      cached.fullName.toLowerCase() === repository.fullName.toLowerCase() &&
      cached.defaultBranch === repository.defaultBranch &&
      !cached.retryable &&
      cached.pushedAt !== null &&
      cached.pushedAt === repository.pushedAt
    ) {
      resolvedById.set(repository.id, { repository, headCommit: cached.headCommit, cached });
    } else {
      needResolution.push(repository);
    }
  }

  const batches = chunk(needResolution, GRAPHQL_BATCH_SIZE);
  const results = await mapConcurrent(batches, NETWORK_CONCURRENCY, async (batch) => {
    const data = await fetchGraphqlData(fetchFn, token, repositoryHeadQuery(batch));
    return batch.flatMap((repository, index) => {
      const raw = data[`repo${index}`];
      if (!isObject(raw)) return [];
      const branch = isObject(raw.defaultBranchRef) ? readString(raw.defaultBranchRef.name) : null;
      const target = isObject(raw.defaultBranchRef?.target) ? raw.defaultBranchRef.target : null;
      const headCommit = isObject(target) ? readString(target.oid) : null;
      if (
        branch !== repository.defaultBranch ||
        !headCommit ||
        !/^[0-9a-f]{40,64}$/i.test(headCommit)
      ) {
        return [];
      }
      return [{ repository, headCommit, cached: cachedById.get(repository.id) ?? null }];
    });
  });
  for (const resolved of results.flat()) {
    resolvedById.set(resolved.repository.id, resolved);
  }

  return {
    resolved: repositories.flatMap((repository) => {
      const value = resolvedById.get(repository.id);
      return value ? [value] : [];
    }),
    unavailableRepositoryCount: repositories.length - resolvedById.size,
  };
}

async function scanChangedRepositories(
  fetchFn: FetchLike,
  token: string,
  repositories: ResolvedRepository[],
): Promise<Map<number, ScanCacheEntry>> {
  const pending = await mapConcurrent(repositories, NETWORK_CONCURRENCY, (resolved) =>
    fetchRepositoryTree(fetchFn, token, resolved),
  );
  const candidates: TreeManifestCandidate[] = [];
  let candidateBytes = 0;
  for (const scan of pending) {
    const scanBytes = scan.candidates.reduce((total, candidate) => total + candidate.size, 0);
    if (
      candidates.length + scan.candidates.length > MAX_TOTAL_MANIFESTS ||
      candidateBytes + scanBytes > MAX_TOTAL_MANIFEST_BYTES
    ) {
      scan.warning = `${scan.resolved.repository.fullName} was skipped because the marketplace scan budget was exhausted.`;
      scan.retryable = true;
      scan.candidates = [];
      continue;
    }
    candidates.push(...scan.candidates);
    candidateBytes += scanBytes;
  }
  const contents = await fetchManifestContents(fetchFn, token, candidates);
  const entries = new Map<number, ScanCacheEntry>();

  for (const scan of pending) {
    const manifests: PluginManifestListing[] = [];
    let invalidManifestCount = scan.invalidManifestCount;
    for (const candidate of scan.candidates) {
      const content = contents.get(manifestCandidateKey(candidate));
      const metadata = typeof content === "string" ? parseManifestSummary(content) : null;
      if (!metadata) {
        invalidManifestCount += 1;
        continue;
      }
      manifests.push({ path: candidate.path, ...metadata });
    }
    const { manifests: uniqueManifests, duplicateCount } = deduplicateManifests(manifests);
    const { repository, headCommit } = scan.resolved;
    entries.set(repository.id, {
      repositoryId: repository.id,
      fullName: repository.fullName,
      defaultBranch: repository.defaultBranch,
      pushedAt: repository.pushedAt,
      headCommit,
      manifestFileCount: scan.manifestFileCount,
      invalidManifestCount,
      duplicateManifestCount: duplicateCount,
      warning: scan.warning,
      retryable: scan.retryable,
      manifests: uniqueManifests,
    });
  }
  return entries;
}

async function fetchRepositoryTree(
  fetchFn: FetchLike,
  token: string,
  resolved: ResolvedRepository,
): Promise<PendingRepositoryScan> {
  const { repository, headCommit } = resolved;
  const url = new URL(
    `${GITHUB_API_URL}/repos/${encodeURIComponent(repository.owner)}/${encodeURIComponent(repository.name)}/git/trees/${headCommit}`,
  );
  url.searchParams.set("recursive", "1");
  const response = await fetchWithTimeout(fetchFn, url, githubRequestInit(token), REQUEST_TIMEOUT_MS);
  if (!response.ok) {
    throw new Error(`GitHub tree fetch failed for ${repository.fullName} with status ${response.status}`);
  }
  const body = await response.json();
  if (!isObject(body) || !Array.isArray(body.tree)) {
    throw new Error(`GitHub tree fetch returned malformed JSON for ${repository.fullName}`);
  }
  if (body.truncated === true || body.tree.length > MAX_TREE_ENTRIES) {
    return skippedRepositoryScan(resolved, `${repository.fullName} was skipped because its Git tree is too large.`);
  }

  const manifestEntries = body.tree
    .map(normalizeTreeEntry)
    .filter((entry): entry is GitHubTreeEntry => entry !== null)
    .filter(
      (entry) =>
        isDiscoverableManifestPath(entry.path) &&
        entry.type === "blob" &&
        REGULAR_BLOB_MODES.has(entry.mode),
    );
  if (manifestEntries.length > MAX_MANIFESTS_PER_REPOSITORY) {
    return skippedRepositoryScan(
      resolved,
      `${repository.fullName} was skipped because it contains more than ${MAX_MANIFESTS_PER_REPOSITORY} plugin manifests.`,
    );
  }

  let invalidManifestCount = 0;
  const candidates: TreeManifestCandidate[] = [];
  for (const entry of manifestEntries) {
    if (
      entry.size === null ||
      entry.size < 0 ||
      entry.size > MANIFEST_MAX_BYTES
    ) {
      invalidManifestCount += 1;
      continue;
    }
    candidates.push({ repository, headCommit, path: entry.path, size: entry.size });
  }

  return {
    resolved,
    manifestFileCount: manifestEntries.length,
    invalidManifestCount,
    warning: null,
    retryable: false,
    candidates,
  };
}

function skippedRepositoryScan(
  resolved: ResolvedRepository,
  warning: string,
): PendingRepositoryScan {
  return {
    resolved,
    manifestFileCount: 0,
    invalidManifestCount: 0,
    warning,
    retryable: false,
    candidates: [],
  };
}

async function fetchManifestContents(
  fetchFn: FetchLike,
  token: string,
  candidates: TreeManifestCandidate[],
): Promise<Map<string, string | null>> {
  const batches = chunk(candidates, GRAPHQL_BATCH_SIZE);
  const results = await mapConcurrent(batches, NETWORK_CONCURRENCY, async (batch) => {
    const data = await fetchGraphqlData(fetchFn, token, manifestContentQuery(batch));
    return batch.flatMap((candidate, index) => {
      const raw = data[`item${index}`];
      const manifest = isObject(raw) && isObject(raw.manifest) ? raw.manifest : null;
      if (!manifest) {
        throw new Error(
          `GitHub manifest fetch omitted ${candidate.repository.fullName}:${candidate.path}`,
        );
      }
      if (manifest.isBinary === true) {
        return [[manifestCandidateKey(candidate), null] as const];
      }
      if (typeof manifest.text !== "string") {
        throw new Error(
          `GitHub manifest fetch returned malformed data for ${candidate.repository.fullName}:${candidate.path}`,
        );
      }
      return [[manifestCandidateKey(candidate), manifest.text] as const];
    });
  });
  return new Map(results.flat());
}

async function fetchGraphqlData(
  fetchFn: FetchLike,
  token: string,
  query: string,
): Promise<Record<string, unknown>> {
  const response = await fetchWithTimeout(
    fetchFn,
    new URL(GITHUB_GRAPHQL_URL),
    {
      ...githubRequestInit(token),
      method: "POST",
      body: JSON.stringify({ query }),
    },
    REQUEST_TIMEOUT_MS,
  );
  if (!response.ok) {
    throw new Error(`GitHub marketplace query failed with status ${response.status}`);
  }
  const body = await response.json();
  if (!isObject(body) || !isObject(body.data)) {
    throw new Error("GitHub marketplace query returned malformed JSON");
  }
  if (
    Array.isArray(body.errors) &&
    body.errors.some((error) => !isMissingAliasGraphqlError(error))
  ) {
    throw new Error("GitHub marketplace query returned GraphQL errors");
  }
  return body.data;
}

function isMissingAliasGraphqlError(error: unknown): boolean {
  if (!isObject(error) || error.type !== "NOT_FOUND" || !Array.isArray(error.path)) {
    return false;
  }
  const alias = error.path[0];
  return typeof alias === "string" && /^(repo|item)\d+$/.test(alias);
}

function repositoryHeadQuery(repositories: RepositoryListing[]): string {
  const selections = repositories.map((repository, index) => {
    const owner = JSON.stringify(repository.owner);
    const name = JSON.stringify(repository.name);
    return `repo${index}: repository(owner: ${owner}, name: ${name}) {
      defaultBranchRef { name target { ... on Commit { oid } } }
    }`;
  });
  return `query PluginMarketplaceHeads {\n${selections.join("\n")}\n}`;
}

function manifestContentQuery(candidates: TreeManifestCandidate[]): string {
  const selections = candidates.map((candidate, index) => {
    const owner = JSON.stringify(candidate.repository.owner);
    const name = JSON.stringify(candidate.repository.name);
    const expression = JSON.stringify(`${candidate.headCommit}:${candidate.path}`);
    return `item${index}: repository(owner: ${owner}, name: ${name}) {
      manifest: object(expression: ${expression}) { ... on Blob { text isBinary } }
    }`;
  });
  return `query PluginMarketplaceManifests {\n${selections.join("\n")}\n}`;
}

export function parseManifestSummary(manifestText: string): Omit<PluginManifestListing, "path"> | null {
  try {
    const manifest = parse(manifestText.replace(/^\uFEFF/, ""));
    if (!isObject(manifest)) return null;
    const id = readTrimmedString(manifest.id);
    const name = readTrimmedString(manifest.name);
    const version = readTrimmedString(manifest.version);
    const minHerdrVersion = readTrimmedString(manifest.min_herdr_version);
    if (
      !id ||
      id.length > 120 ||
      !/^[A-Za-z0-9:._-]+$/.test(id) ||
      !name ||
      name.length > PLUGIN_NAME_MAX_CHARS ||
      !version ||
      version.length > PLUGIN_VERSION_MAX_CHARS ||
      !minHerdrVersion ||
      minHerdrVersion.length > PLUGIN_VERSION_MAX_CHARS ||
      !isHerdrVersion(minHerdrVersion)
    ) {
      return null;
    }

    let description: string | null = null;
    if (manifest.description !== undefined) {
      if (typeof manifest.description !== "string") return null;
      description = manifest.description.trim() || null;
      if (description && description.length > PLUGIN_DESCRIPTION_MAX_CHARS) return null;
    }

    let platforms: string[] | null = null;
    if (manifest.platforms !== undefined) {
      if (
        !Array.isArray(manifest.platforms) ||
        manifest.platforms.length === 0 ||
        manifest.platforms.length > PLUGIN_PLATFORMS.size ||
        !manifest.platforms.every(
          (platform) => typeof platform === "string" && PLUGIN_PLATFORMS.has(platform),
        )
      ) {
        return null;
      }
      platforms = [...manifest.platforms];
      if (new Set(platforms).size !== platforms.length) return null;
    }

    return { id, name, version, minHerdrVersion, description, platforms };
  } catch {
    return null;
  }
}

async function readScanCache(
  bucket: R2Bucket,
  logger: Pick<Console, "error">,
): Promise<ScanCache> {
  const empty = (): ScanCache => ({ schemaVersion: SCAN_CACHE_SCHEMA_VERSION, entries: [] });
  const object = await bucket.get(SCAN_CACHE_KEY);
  if (!object) return empty();
  try {
    const value: unknown = JSON.parse(await object.text());
    if (
      !isObject(value) ||
      value.schemaVersion !== SCAN_CACHE_SCHEMA_VERSION ||
      !Array.isArray(value.entries)
    ) {
      throw new Error("unsupported cache shape");
    }
    const entries = value.entries.map(readScanCacheEntry);
    if (entries.some((entry) => entry === null)) {
      throw new Error("invalid cache entry");
    }
    return { schemaVersion: SCAN_CACHE_SCHEMA_VERSION, entries: entries as ScanCacheEntry[] };
  } catch (error) {
    const message = error instanceof Error ? error.message : "unknown cache error";
    logger.error(`discarding invalid plugin marketplace scan cache: ${message}`);
    return empty();
  }
}

function readScanCacheEntry(value: unknown): ScanCacheEntry | null {
  if (!isObject(value) || !Array.isArray(value.manifests)) return null;
  const repositoryId = readInteger(value.repositoryId);
  const fullName = readString(value.fullName);
  const defaultBranch = readString(value.defaultBranch);
  const pushedAt = value.pushedAt === null ? null : readIsoString(value.pushedAt);
  const headCommit = readString(value.headCommit);
  const manifestFileCount = readNonNegativeIntegerOrNull(value.manifestFileCount);
  const invalidManifestCount = readNonNegativeIntegerOrNull(value.invalidManifestCount);
  const duplicateManifestCount = readNonNegativeIntegerOrNull(value.duplicateManifestCount);
  const warning = value.warning === null ? null : readString(value.warning);
  const retryable = typeof value.retryable === "boolean" ? value.retryable : null;
  const manifests = value.manifests.map(readCachedManifest);
  if (
    repositoryId === null ||
    !fullName ||
    !defaultBranch ||
    (value.pushedAt !== null && pushedAt === null) ||
    !headCommit ||
    !/^[0-9a-f]{40,64}$/i.test(headCommit) ||
    manifestFileCount === null ||
    invalidManifestCount === null ||
    duplicateManifestCount === null ||
    retryable === null ||
    (value.warning !== null && !warning) ||
    manifests.some((manifest) => manifest === null)
  ) {
    return null;
  }
  return {
    repositoryId,
    fullName,
    defaultBranch,
    pushedAt,
    headCommit,
    manifestFileCount,
    invalidManifestCount,
    duplicateManifestCount,
    warning,
    retryable,
    manifests: manifests as PluginManifestListing[],
  };
}

function readCachedManifest(value: unknown): PluginManifestListing | null {
  if (!isObject(value)) return null;
  const path = readString(value.path);
  const id = readString(value.id);
  const name = readString(value.name);
  const version = readString(value.version);
  const minHerdrVersion = readString(value.minHerdrVersion);
  const description = value.description === null ? null : readString(value.description);
  const platforms = value.platforms === null ? null : readStringArrayOrNull(value.platforms);
  if (
    !path ||
    !isManifestPath(path) ||
    !id ||
    !name ||
    !version ||
    !minHerdrVersion ||
    (value.description !== null && !description) ||
    (value.platforms !== null && platforms === null)
  ) {
    return null;
  }
  return { path, id, name, version, minHerdrVersion, description, platforms };
}

export function normalizeRepositories(repositories: GitHubRepository[]): RepositoryListing[] {
  const byId = new Map<number, RepositoryListing>();
  for (const repository of repositories.map(normalizeRepository)) {
    if (repository && !byId.has(repository.id)) byId.set(repository.id, repository);
  }
  return [...byId.values()].sort(compareRepositories);
}

async function readBlacklistedRepositories(env: Env): Promise<Set<string>> {
  const kv = env.PLUGIN_MARKETPLACE_BLACKLIST;
  const blockedRepositories = new Set<string>();
  if (!kv) return blockedRepositories;

  let cursor: string | undefined;
  do {
    const page = await kv.list({ prefix: BLACKLIST_REPO_KEY_PREFIX, cursor });
    for (const key of page.keys) {
      const repository = key.name.slice(BLACKLIST_REPO_KEY_PREFIX.length).trim().toLowerCase();
      if (repository.includes("/")) blockedRepositories.add(repository);
    }
    cursor = page.cursor;
  } while (cursor);
  return blockedRepositories;
}

function normalizeRepository(repo: GitHubRepository): RepositoryListing | null {
  if (
    readBoolean(repo.disabled) ||
    readBoolean(repo.archived) ||
    readBoolean(repo.fork) ||
    readBoolean(repo.private) ||
    readString(repo.visibility) === "private"
  ) {
    return null;
  }

  const fullNameParts = splitFullName(readString(repo.full_name));
  const ownerObject = isObject(repo.owner) ? repo.owner : {};
  const owner = firstString(readString(ownerObject.login), fullNameParts.owner);
  const name = firstString(readString(repo.name), fullNameParts.name);
  const defaultBranch = readString(repo.default_branch);
  if (!owner || !name || !defaultBranch) return null;

  const fullName = readString(repo.full_name) ?? `${owner}/${name}`;
  const url = readString(repo.html_url);
  const id = readInteger(repo.id);
  if (!url || !isValidGitHubRepoUrl(url, owner, name) || id === null || id <= 0) return null;

  return {
    id,
    fullName,
    owner,
    name,
    description: readNullableString(repo.description),
    url,
    stars: readNonNegativeInteger(repo.stargazers_count),
    forks: readNonNegativeInteger(repo.forks_count),
    openIssues: readNonNegativeInteger(repo.open_issues_count),
    language: readNullableString(repo.language),
    topics: readStringArray(repo.topics),
    createdAt: readIsoString(repo.created_at),
    updatedAt: readIsoString(repo.updated_at),
    pushedAt: readIsoString(repo.pushed_at),
    defaultBranch,
  };
}

function normalizeTreeEntry(value: unknown): GitHubTreeEntry | null {
  if (!isObject(value)) return null;
  const path = readString(value.path);
  const mode = readString(value.mode);
  const type = readString(value.type);
  if (!path || !mode || !type) return null;
  return { path, mode, type, size: readNonNegativeIntegerOrNull(value.size) };
}

function isManifestPath(path: string): boolean {
  return path === PLUGIN_MANIFEST_FILE || path.endsWith(`/${PLUGIN_MANIFEST_FILE}`);
}

function isDiscoverableManifestPath(path: string): boolean {
  if (!isManifestPath(path)) return false;
  const ignoredSegments = new Set([
    "test",
    "tests",
    "fixture",
    "fixtures",
    "testdata",
    "__fixtures__",
  ]);
  return !path
    .split("/")
    .slice(0, -1)
    .some((segment) => ignoredSegments.has(segment.toLowerCase()));
}

function deduplicateManifests(manifests: PluginManifestListing[]): {
  manifests: PluginManifestListing[];
  duplicateCount: number;
} {
  const preferred = [...manifests].sort((a, b) => {
    const depth = (path: string) => path.split("/").length;
    return depth(a.path) - depth(b.path) || a.path.localeCompare(b.path);
  });
  const byId = new Map<string, PluginManifestListing>();
  let duplicateCount = 0;
  for (const manifest of preferred) {
    if (byId.has(manifest.id)) {
      duplicateCount += 1;
    } else {
      byId.set(manifest.id, manifest);
    }
  }
  return {
    manifests: [...byId.values()].sort((a, b) => a.path.localeCompare(b.path)),
    duplicateCount,
  };
}

function manifestCandidateKey(candidate: TreeManifestCandidate): string {
  return `${candidate.repository.id}:${candidate.path}`;
}

function compareRepositories(a: RepositoryListing, b: RepositoryListing): number {
  return (
    b.stars - a.stars ||
    dateMs(b.pushedAt) - dateMs(a.pushedAt) ||
    a.fullName.localeCompare(b.fullName)
  );
}

function githubRequestInit(token: string): RequestInit {
  return {
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${token}`,
      "User-Agent": "herdr-plugin-marketplace",
      "X-GitHub-Api-Version": GITHUB_API_VERSION,
    },
  };
}

async function fetchWithTimeout(
  fetchFn: FetchLike,
  url: URL,
  init: RequestInit,
  timeoutMs: number,
): Promise<Response> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetchFn(url, { ...init, signal: controller.signal });
  } finally {
    clearTimeout(timeout);
  }
}

function chunk<T>(values: T[], size: number): T[][] {
  const chunks: T[][] = [];
  for (let index = 0; index < values.length; index += size) {
    chunks.push(values.slice(index, index + size));
  }
  return chunks;
}

async function mapConcurrent<T, R>(
  values: T[],
  concurrency: number,
  operation: (value: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(values.length);
  let nextIndex = 0;
  const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    while (nextIndex < values.length) {
      const index = nextIndex;
      nextIndex += 1;
      results[index] = await operation(values[index]);
    }
  });
  await Promise.all(workers);
  return results;
}

function jsonResponse(body: unknown, status: number, cacheControl: string): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": cacheControl,
    },
  });
}

function isValidGitHubRepoUrl(url: string, owner: string, name: string): boolean {
  try {
    const parsed = new URL(url);
    const segments = parsed.pathname.split("/").filter(Boolean);
    return (
      parsed.protocol === "https:" &&
      parsed.hostname === "github.com" &&
      segments.length === 2 &&
      segments[0].toLowerCase() === owner.toLowerCase() &&
      segments[1].toLowerCase() === name.toLowerCase()
    );
  } catch {
    return false;
  }
}

function splitFullName(fullName: string | null): { owner: string | null; name: string | null } {
  if (!fullName) return { owner: null, name: null };
  const [owner, name, extra] = fullName.split("/");
  return owner && name && !extra ? { owner, name } : { owner: null, name: null };
}

function firstString(...values: Array<string | null>): string | null {
  return values.find((value) => value !== null) ?? null;
}

function readString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function readTrimmedString(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
}

function readNullableString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function readStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function readStringArrayOrNull(value: unknown): string[] | null {
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    ? value
    : null;
}

function readInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) ? value : null;
}

function readNonNegativeInteger(value: unknown): number {
  return readNonNegativeIntegerOrNull(value) ?? 0;
}

function readNonNegativeIntegerOrNull(value: unknown): number | null {
  const integer = readInteger(value);
  return integer !== null && integer >= 0 ? integer : null;
}

function readBoolean(value: unknown): boolean {
  return value === true;
}

function readIsoString(value: unknown): string | null {
  return typeof value === "string" && !Number.isNaN(Date.parse(value)) ? value : null;
}

function isHerdrVersion(value: string): boolean {
  const normalized = value.startsWith("v") ? value.slice(1) : value;
  const parts = normalized.split(".");
  return (
    parts.length === 3 &&
    parts.every((part) => /^\d+$/.test(part) && Number(part) <= 0xffff_ffff)
  );
}

function dateMs(value: string | null): number {
  return value ? Date.parse(value) || 0 : 0;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
