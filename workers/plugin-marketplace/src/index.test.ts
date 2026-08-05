import { describe, expect, test } from "bun:test";
import worker, {
  normalizeRepositories,
  parseManifestSummary,
  refreshPlugins,
  type Env,
} from "./index";

const HEAD_COMMIT = "a".repeat(40);
const SECOND_COMMIT = "b".repeat(40);

type TreeFixture = {
  path: string;
  content?: string;
  mode?: string;
  type?: string;
  size?: number;
};

class MemoryR2 {
  objects = new Map<string, { value: string; options: unknown }>();
  async put(key: string, value: string, options?: unknown): Promise<void> {
    this.objects.set(key, { value, options });
  }

  async get(key: string): Promise<{ text(): Promise<string> } | null> {
    const object = this.objects.get(key);
    return object
      ? {
          async text() {
            return object.value;
          },
        }
      : null;
  }
}

class MemoryKV {
  constructor(private readonly keyNames: string[]) {}

  async list(options?: { prefix?: string }): Promise<{ keys: Array<{ name: string }> }> {
    return {
      keys: this.keyNames
        .filter((name) => !options?.prefix || name.startsWith(options.prefix))
        .map((name) => ({ name })),
    };
  }
}

function repo(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 1,
    full_name: "ogulcancelik/herdr-plugin-example",
    owner: { login: "ogulcancelik" },
    name: "herdr-plugin-example",
    description: "Example plugin repository",
    html_url: "https://github.com/ogulcancelik/herdr-plugin-example",
    default_branch: "main",
    stargazers_count: 5,
    forks_count: 1,
    open_issues_count: 0,
    language: "TypeScript",
    topics: ["herdr-plugin"],
    created_at: "2026-06-01T00:00:00Z",
    updated_at: "2026-06-02T00:00:00Z",
    pushed_at: "2026-06-03T00:00:00Z",
    archived: false,
    fork: false,
    disabled: false,
    private: false,
    visibility: "public",
    ...overrides,
  };
}

function manifest(overrides = ""): string {
  return `
id = "example.plugin"
name = "Example Plugin"
version = "0.2.0"
min_herdr_version = "0.7.0"
description = "Example manifest"
platforms = ["linux", "macos"]
${overrides}`;
}

function env(bucket = new MemoryR2(), blacklist?: MemoryKV): Env {
  return {
    PLUGIN_MARKETPLACE_BUCKET: bucket,
    PLUGIN_MARKETPLACE_BLACKLIST: blacklist,
    GITHUB_TOKEN: "token",
  };
}

function repositoryFetch(options: {
  repositories: Record<string, unknown>[];
  trees?: Record<string, TreeFixture[]>;
  commits?: Record<string, string>;
  totalCount?: number;
  incompleteResults?: boolean;
  searchStatus?: number;
  treeStatus?: Record<string, number>;
  truncatedTrees?: Set<string>;
  onRequest?: (kind: "search" | "head" | "tree" | "manifest", detail: string) => void;
}): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = new URL(input.toString());
    if (url.pathname === "/search/repositories") {
      options.onRequest?.("search", url.toString());
      if (options.searchStatus) return new Response("search failed", { status: options.searchStatus });
      return Response.json({
        total_count: options.totalCount ?? options.repositories.length,
        incomplete_results: options.incompleteResults ?? false,
        items: options.repositories,
      });
    }

    const treeMatch = url.pathname.match(/^\/repos\/([^/]+)\/([^/]+)\/git\/trees\/([^/]+)$/);
    if (treeMatch) {
      const fullName = `${decodeURIComponent(treeMatch[1])}/${decodeURIComponent(treeMatch[2])}`;
      options.onRequest?.("tree", fullName);
      const status = options.treeStatus?.[fullName];
      if (status) return new Response("tree failed", { status });
      const fixtures = options.trees?.[fullName] ?? [
        { path: "herdr-plugin.toml", content: manifest() },
      ];
      return Response.json({
        sha: "tree-sha",
        truncated: options.truncatedTrees?.has(fullName) ?? false,
        tree: fixtures.map((fixture) => ({
          path: fixture.path,
          mode: fixture.mode ?? "100644",
          type: fixture.type ?? "blob",
          size:
            fixture.size ??
            new TextEncoder().encode(fixture.content ?? "").byteLength,
        })),
      });
    }

    if (url.pathname === "/graphql") {
      const request = JSON.parse(String(init?.body ?? "{}"));
      const query = String(request.query ?? "");
      const data: Record<string, unknown> = {};
      if (query.includes("PluginMarketplaceHeads")) {
        options.onRequest?.("head", query);
        for (const match of query.matchAll(
          /repo(\d+): repository\(owner: "([^"]+)", name: "([^"]+)"\)/g,
        )) {
          const [, alias, owner, name] = match;
          const fullName = `${owner}/${name}`;
          const repository = options.repositories.find(
            (candidate) => candidate.full_name === fullName,
          );
          data[`repo${alias}`] = repository
            ? {
                defaultBranchRef: {
                  name: repository.default_branch ?? "main",
                  target: { oid: options.commits?.[fullName] ?? HEAD_COMMIT },
                },
              }
            : null;
        }
      } else if (query.includes("PluginMarketplaceManifests")) {
        options.onRequest?.("manifest", query);
        for (const match of query.matchAll(
          /item(\d+): repository\(owner: "([^"]+)", name: "([^"]+)"\) \{\s+manifest: object\(expression: "[a-f0-9]+:([^"]+)"\)/g,
        )) {
          const [, alias, owner, name, path] = match;
          const fullName = `${owner}/${name}`;
          const fixture = (options.trees?.[fullName] ?? [
            { path: "herdr-plugin.toml", content: manifest() },
          ]).find((entry) => entry.path === path);
          data[`item${alias}`] = fixture?.content === undefined
            ? { manifest: null }
            : { manifest: { text: fixture.content } };
        }
      } else {
        throw new Error(`unexpected GraphQL query: ${query}`);
      }
      return Response.json({ data });
    }

    throw new Error(`unexpected request: ${url}`);
  }) as typeof fetch;
}

describe("normalizeRepositories", () => {
  test("normalizes fields while preserving repository-card ordering", () => {
    const plugins = normalizeRepositories([
      repo({
        id: 2,
        full_name: "other/newer",
        owner: { login: "other" },
        name: "newer",
        html_url: "https://github.com/other/newer",
        stargazers_count: 5,
        pushed_at: "2026-06-04T00:00:00Z",
      }),
      repo(),
    ]);

    expect(plugins.map((plugin) => plugin.fullName)).toEqual([
      "other/newer",
      "ogulcancelik/herdr-plugin-example",
    ]);
    expect(plugins[1]).toMatchObject({
      id: 1,
      owner: "ogulcancelik",
      name: "herdr-plugin-example",
      defaultBranch: "main",
      stars: 5,
    });
  });

  test("deduplicates repositories by immutable GitHub id", () => {
    const plugins = normalizeRepositories([
      repo(),
      repo({ full_name: "duplicate/name", owner: { login: "duplicate" }, name: "name", html_url: "https://github.com/duplicate/name" }),
    ]);
    expect(plugins).toHaveLength(1);
    expect(plugins[0].fullName).toBe("ogulcancelik/herdr-plugin-example");
  });

  test("drops unsafe, unavailable, and default-branch-less repositories", () => {
    const plugins = normalizeRepositories([
      repo({ html_url: "https://example.com/ogulcancelik/herdr-plugin-example" }),
      repo({ archived: true }),
      repo({ fork: true }),
      repo({ disabled: true }),
      repo({ private: true }),
      repo({ visibility: "private" }),
      repo({ default_branch: undefined }),
      repo({ id: 5 }),
    ]);
    expect(plugins.map((plugin) => plugin.id)).toEqual([5]);
  });
});

describe("parseManifestSummary", () => {
  test("extracts metadata and accepts the UTF-8 BOM accepted by Herdr", () => {
    expect(parseManifestSummary(`\uFEFF${manifest()}`)).toEqual({
      id: "example.plugin",
      name: "Example Plugin",
      version: "0.2.0",
      minHerdrVersion: "0.7.0",
      description: "Example manifest",
      platforms: ["linux", "macos"],
    });
  });

  test("rejects malformed TOML and invalid required metadata", () => {
    expect(parseManifestSummary("not = [valid")).toBeNull();
    expect(parseManifestSummary(manifest().replace("example.plugin", "bad/plugin"))).toBeNull();
    expect(parseManifestSummary(manifest().replace("0.7.0", "next"))).toBeNull();
    expect(parseManifestSummary(manifest().replace('["linux", "macos"]', "[]"))).toBeNull();
    expect(
      parseManifestSummary(manifest().replace("Example Plugin", "n".repeat(121))),
    ).toBeNull();
  });
});

describe("refreshPlugins", () => {
  test("publishes one backward-compatible repository card with multiple manifests", async () => {
    const bucket = new MemoryR2();
    const fullName = "ogulcancelik/herdr-plugin-example";
    const fetch = repositoryFetch({
      repositories: [repo()],
      trees: {
        [fullName]: [
          { path: "herdr-plugin.toml", content: manifest() },
          {
            path: "plugins/second/herdr-plugin.toml",
            content: manifest().replace("example.plugin", "example.second").replace("Example Plugin", "Second Plugin"),
          },
        ],
      },
    });

    const result = await refreshPlugins(env(bucket), {
      fetch,
      now: new Date("2026-06-20T12:00:00.000Z"),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    const snapshotObject = bucket.objects.get("plugins/index.json");
    const snapshot = JSON.parse(snapshotObject?.value ?? "");
    expect(snapshotObject?.options).toEqual({
      httpMetadata: {
        contentType: "application/json; charset=utf-8",
        cacheControl: "public, max-age=300, s-maxage=1800, stale-while-revalidate=3600",
      },
    });
    expect(snapshot).toMatchObject({
      schemaVersion: 1,
      generatedAt: "2026-06-20T12:00:00.000Z",
      pluginCount: 2,
      repositoryCount: 1,
      source: {
        missingManifestCount: 0,
        invalidManifestCount: 0,
      },
    });
    expect(snapshot.plugins[0]).toMatchObject({
      id: 1,
      fullName,
      name: "herdr-plugin-example",
      headCommit: HEAD_COMMIT,
      manifests: [
        { path: "herdr-plugin.toml", id: "example.plugin" },
        { path: "plugins/second/herdr-plugin.toml", id: "example.second" },
      ],
    });
    expect(snapshot.plugins[0]).not.toHaveProperty("defaultBranch");
    expect(bucket.objects.has("plugins/scan-cache.json")).toBe(true);
    expect(
      [...bucket.objects.keys()].some((key) => key.includes("history")),
    ).toBe(false);
  });

  test("reuses cached manifests without resolving or rescanning an unchanged repository", async () => {
    const bucket = new MemoryR2();
    const requests: string[] = [];
    const fetch = repositoryFetch({
      repositories: [repo()],
      onRequest(kind) {
        requests.push(kind);
      },
    });

    expect((await refreshPlugins(env(bucket), { fetch, logger: { error() {} } })).ok).toBe(true);
    requests.length = 0;
    expect((await refreshPlugins(env(bucket), { fetch, logger: { error() {} } })).ok).toBe(true);

    expect(requests).toEqual(["search"]);
  });

  test("rescans a repository when its pushed timestamp and head change", async () => {
    const bucket = new MemoryR2();
    const repository = repo();
    const fullName = String(repository.full_name);
    const commits = { [fullName]: HEAD_COMMIT };
    const trees = { [fullName]: [{ path: "herdr-plugin.toml", content: manifest() }] };
    const fetch = repositoryFetch({ repositories: [repository], commits, trees });

    expect((await refreshPlugins(env(bucket), { fetch, logger: { error() {} } })).ok).toBe(true);
    commits[fullName] = SECOND_COMMIT;
    repository.pushed_at = "2026-06-04T00:00:00Z";
    trees[fullName][0].content = manifest().replace("0.2.0", "0.3.0");

    const result = await refreshPlugins(env(bucket), { fetch, logger: { error() {} } });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.plugins[0].headCommit).toBe(SECOND_COMMIT);
    expect(result.snapshot.plugins[0].manifests[0].version).toBe("0.3.0");
  });

  test("counts manifests separately from repository cards and omits empty cards", async () => {
    const repositories = [
      repo(),
      repo({
        id: 2,
        full_name: "example/empty",
        owner: { login: "example" },
        name: "empty",
        html_url: "https://github.com/example/empty",
      }),
      repo({
        id: 3,
        full_name: "example/invalid",
        owner: { login: "example" },
        name: "invalid",
        html_url: "https://github.com/example/invalid",
      }),
    ];
    const result = await refreshPlugins(env(), {
      fetch: repositoryFetch({
        repositories,
        trees: {
          "ogulcancelik/herdr-plugin-example": [
            { path: "one/herdr-plugin.toml", content: manifest() },
            { path: "two/herdr-plugin.toml", content: manifest().replace("example.plugin", "example.two") },
          ],
          "example/empty": [{ path: "README.md", content: "empty" }],
          "example/invalid": [{ path: "herdr-plugin.toml", content: "id = [broken" }],
        },
      }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.pluginCount).toBe(2);
    expect(result.snapshot.repositoryCount).toBe(1);
    expect(result.snapshot.source.missingManifestCount).toBe(1);
    expect(result.snapshot.source.invalidManifestCount).toBe(1);
  });

  test("ignores manifest symlinks and indexes their regular target", async () => {
    const fullName = "ogulcancelik/herdr-plugin-example";
    const result = await refreshPlugins(env(), {
      fetch: repositoryFetch({
        repositories: [repo()],
        trees: {
          [fullName]: [
            { path: "herdr-plugin.toml", content: "plugin/herdr-plugin.toml", mode: "120000" },
            { path: "plugin/herdr-plugin.toml", content: manifest() },
          ],
        },
      }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.plugins[0].manifests.map((item) => item.path)).toEqual([
      "plugin/herdr-plugin.toml",
    ]);
    expect(result.snapshot.source.invalidManifestCount).toBe(0);
  });

  test("ignores test fixtures and deduplicates repeated plugin ids", async () => {
    const fullName = "ogulcancelik/herdr-plugin-example";
    const result = await refreshPlugins(env(), {
      fetch: repositoryFetch({
        repositories: [repo()],
        trees: {
          [fullName]: [
            { path: "herdr-plugin.toml", content: manifest() },
            { path: "platform/herdr-plugin.toml", content: manifest() },
            {
              path: "tests/fixtures/helper/herdr-plugin.toml",
              content: manifest().replace("example.plugin", "example.fixture"),
            },
          ],
        },
      }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.plugins[0].manifests.map((item) => item.path)).toEqual([
      "herdr-plugin.toml",
    ]);
    expect(result.snapshot.source.duplicateManifestCount).toBe(1);
  });

  test("skips a truncated tree without blocking the marketplace", async () => {
    const fullName = "ogulcancelik/herdr-plugin-example";
    const result = await refreshPlugins(env(), {
      fetch: repositoryFetch({
        repositories: [repo()],
        truncatedTrees: new Set([fullName]),
      }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.plugins).toEqual([]);
    expect(result.snapshot.source.skippedRepositoryCount).toBe(1);
    expect(result.snapshot.source.warnings?.[0]).toContain(fullName);
  });

  test("writes an empty snapshot when every repository is blacklisted", async () => {
    const bucket = new MemoryR2();
    const result = await refreshPlugins(
      env(bucket, new MemoryKV(["repo:ogulcancelik/herdr-plugin-example"])),
      { fetch: repositoryFetch({ repositories: [repo()] }), logger: { error() {} } },
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.plugins).toEqual([]);
    expect(result.snapshot.source.blacklistedCount).toBe(1);
  });

  test("pins tree and manifest reads to the resolved commit", async () => {
    const requests: Array<[string, string]> = [];
    const result = await refreshPlugins(env(), {
      fetch: repositoryFetch({
        repositories: [repo()],
        onRequest(kind, detail) {
          requests.push([kind, detail]);
        },
      }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    expect(requests.find(([kind]) => kind === "tree")?.[1]).toBe(
      "ogulcancelik/herdr-plugin-example",
    );
    const manifestQuery = requests.find(([kind]) => kind === "manifest")?.[1] ?? "";
    expect(manifestQuery).toContain(`${HEAD_COMMIT}:herdr-plugin.toml`);
    expect(manifestQuery).not.toContain("main:herdr-plugin.toml");
  });

  test("does not overwrite the public snapshot when scanning fails", async () => {
    const bucket = new MemoryR2();
    await bucket.put("plugins/index.json", '{"schemaVersion":1,"plugins":[{"id":1}]}');
    const result = await refreshPlugins(env(bucket), {
      fetch: repositoryFetch({
        repositories: [repo()],
        treeStatus: { "ogulcancelik/herdr-plugin-example": 429 },
      }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(false);
    expect(bucket.objects.get("plugins/index.json")?.value).toBe(
      '{"schemaVersion":1,"plugins":[{"id":1}]}',
    );
  });

  test("discards malformed cache state and rebuilds it", async () => {
    const bucket = new MemoryR2();
    await bucket.put("plugins/index.json", '{"schemaVersion":1,"plugins":[{"id":1}]}');
    await bucket.put("plugins/scan-cache.json", "broken");
    const errors: string[] = [];

    const result = await refreshPlugins(env(bucket), {
      fetch: repositoryFetch({ repositories: [repo()] }),
      logger: { error(message) { errors.push(String(message)); } },
    });

    expect(result.ok).toBe(true);
    expect(errors[0]).toContain("discarding invalid plugin marketplace scan cache");
    expect(JSON.parse(bucket.objects.get("plugins/scan-cache.json")?.value ?? "").entries).toHaveLength(1);
  });

  test("rejects an implausible empty search when a healthy cache exists", async () => {
    const bucket = new MemoryR2();
    const normalFetch = repositoryFetch({ repositories: [repo()] });
    expect((await refreshPlugins(env(bucket), { fetch: normalFetch, logger: { error() {} } })).ok).toBe(true);
    const previous = bucket.objects.get("plugins/index.json")?.value;

    const result = await refreshPlugins(env(bucket), {
      fetch: repositoryFetch({ repositories: [] }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(false);
    expect(bucket.objects.get("plugins/index.json")?.value).toBe(previous);
  });

  test("publishes an empty snapshot when the initial complete search is empty", async () => {
    const bucket = new MemoryR2();
    const result = await refreshPlugins(env(bucket), {
      fetch: repositoryFetch({ repositories: [] }),
      logger: { error() {} },
    });

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.plugins).toEqual([]);
    expect(result.snapshot.pluginCount).toBe(0);
  });

  test("preserves the snapshot when head resolution omits a cached repository", async () => {
    const bucket = new MemoryR2();
    const repository = repo();
    const baseFetch = repositoryFetch({ repositories: [repository] });
    expect((await refreshPlugins(env(bucket), { fetch: baseFetch, logger: { error() {} } })).ok).toBe(true);
    const previous = bucket.objects.get("plugins/index.json")?.value;

    repository.pushed_at = "2026-06-04T00:00:00Z";
    const missingHeadFetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const request = String(init?.body ?? "");
      if (new URL(input.toString()).pathname === "/graphql" && request.includes("PluginMarketplaceHeads")) {
        return Response.json({
          data: { repo0: null },
          errors: [{ type: "NOT_FOUND", path: ["repo0"], message: "not found" }],
        });
      }
      return baseFetch(input, init);
    };
    const result = await refreshPlugins(env(bucket), {
      fetch: missingHeadFetch as typeof fetch,
      logger: { error() {} },
    });

    expect(result.ok).toBe(false);
    expect(bucket.objects.get("plugins/index.json")?.value).toBe(previous);
  });

  test("does not cache a transiently missing manifest response", async () => {
    const bucket = new MemoryR2();
    const repository = repo();
    const fullName = String(repository.full_name);
    const commits = { [fullName]: HEAD_COMMIT };
    const baseFetch = repositoryFetch({ repositories: [repository], commits });
    expect((await refreshPlugins(env(bucket), { fetch: baseFetch, logger: { error() {} } })).ok).toBe(true);
    const previous = bucket.objects.get("plugins/index.json")?.value;

    repository.pushed_at = "2026-06-04T00:00:00Z";
    commits[fullName] = SECOND_COMMIT;
    const missingFetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const request = String(init?.body ?? "");
      if (new URL(input.toString()).pathname === "/graphql" && request.includes("PluginMarketplaceManifests")) {
        return Response.json({
          data: { item0: { manifest: null } },
          errors: [{ type: "NOT_FOUND", path: ["item0"], message: "not found" }],
        });
      }
      return baseFetch(input, init);
    };
    const result = await refreshPlugins(env(bucket), {
      fetch: missingFetch as typeof fetch,
      logger: { error() {} },
    });

    expect(result.ok).toBe(false);
    expect(bucket.objects.get("plugins/index.json")?.value).toBe(previous);
  });

  test("retries budget-skipped repositories when capacity becomes available", async () => {
    const repositories = Array.from({ length: 51 }, (_, index) =>
      repo({
        id: index + 1,
        full_name: `owner/repo-${index}`,
        owner: { login: "owner" },
        name: `repo-${index}`,
        html_url: `https://github.com/owner/repo-${index}`,
      }),
    );
    const trees = Object.fromEntries(
      repositories.map((repository) => [
        repository.full_name,
        Array.from({ length: 100 }, (_, index) => ({
          path: `plugins/${index}/herdr-plugin.toml`,
          content: manifest(),
        })),
      ]),
    );

    const bucket = new MemoryR2();
    const fetch = repositoryFetch({ repositories, trees });
    const result = await refreshPlugins(env(bucket), { fetch, logger: { error() {} } });

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.snapshot.source.skippedRepositoryCount).toBe(1);
    expect(result.snapshot.source.warnings?.[0]).toContain("scan budget was exhausted");

    repositories.shift();
    const recovered = await refreshPlugins(env(bucket), { fetch, logger: { error() {} } });
    expect(recovered.ok).toBe(true);
    if (!recovered.ok) return;
    expect(recovered.snapshot.source.skippedRepositoryCount).toBe(0);
    expect(recovered.snapshot.repositoryCount).toBe(50);
  });

  for (const { name, fetch } of [
    {
      name: "GitHub failure",
      fetch: repositoryFetch({ repositories: [], searchStatus: 429 }),
    },
    {
      name: "incomplete search results",
      fetch: repositoryFetch({ repositories: [repo()], incompleteResults: true }),
    },
  ]) {
    test(`preserves the current snapshot on ${name}`, async () => {
      const bucket = new MemoryR2();
      await bucket.put("plugins/index.json", '{"schemaVersion":1,"plugins":[{"id":1}]}');
      const result = await refreshPlugins(env(bucket), { fetch, logger: { error() {} } });
      expect(result.ok).toBe(false);
      expect(bucket.objects.get("plugins/index.json")?.value).toBe(
        '{"schemaVersion":1,"plugins":[{"id":1}]}',
      );
    });
  }
});

describe("fetch handler", () => {
  test("does not expose a public Worker API", async () => {
    const response = await worker.fetch(new Request("https://herdr.dev/api/plugins"), env());
    expect(response.status).toBe(404);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
  });
});
