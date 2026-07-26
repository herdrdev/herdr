import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { SessionManager } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { cp, mkdir, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SOURCE_CHECKOUT = "/home/can/Projects/herdr";
const DEFAULT_BASE = "master";
const EXTENSION_DIR = dirname(fileURLToPath(import.meta.url));
const EXTENSION_NAME = "herdr-worktree";

type HerdrWorktreeResult = {
  result?: {
    workspace?: { workspace_id?: string; label?: string };
    tab?: { tab_id?: string };
    root_pane?: { pane_id?: string };
    worktree?: { path?: string; branch?: string; label?: string };
  };
  error?: { code?: string; message?: string };
};

type StartOptions = {
  branch?: string;
  base?: string;
  label?: string;
  sourceCheckout?: string;
  closeOldPane: boolean;
  copyExtension: boolean;
};

export default function (pi: ExtensionAPI) {
  pi.registerTool({
    name: "herdr_start_worktree",
    label: "Start Herdr Worktree",
    description:
      "Create a Herdr-linked git worktree from the Herdr master checkout, continue the active pi session in it, " +
      "start pi in the new Herdr pane, then shut down and clean up the old pane.",
    promptSnippet: "Create a Herdr worktree workspace and continue the active pi session in it",
    promptGuidelines: [
      "Use herdr_start_worktree when work in the Herdr repo should continue in a fresh git worktree.",
      "herdr_start_worktree creates the checkout from /home/can/Projects/herdr on master by default.",
      "Prefer passing a clear branch name such as issue/123-short-slug when the work relates to an issue.",
      "After herdr_start_worktree succeeds, the current pi process will shut down and the old Herdr pane will close.",
    ],
    parameters: Type.Object({
      branch: Type.Optional(
        Type.String({
          description:
            "Branch name for the new worktree. If omitted, Herdr generates a worktree/* branch.",
        }),
      ),
      base: Type.Optional(
        Type.String({
          description: "Base ref for the new worktree. Defaults to master.",
        }),
      ),
      label: Type.Optional(
        Type.String({
          description: "Workspace label for the new Herdr worktree workspace.",
        }),
      ),
      closeOldPane: Type.Optional(
        Type.Boolean({
          description: "Close the old Herdr pane after the old pi process exits. Defaults to true.",
        }),
      ),
      copyExtension: Type.Optional(
        Type.Boolean({
          description:
            "Copy this project-local extension into the new worktree before starting pi there. Defaults to true.",
        }),
      ),
    }),
    async execute(_toolCallId, params, signal, _onUpdate, ctx) {
      return startHerdrWorktree(pi, ctx, signal, {
        branch: cleanOptional(params.branch),
        base: cleanOptional(params.base) ?? DEFAULT_BASE,
        label: cleanOptional(params.label),
        sourceCheckout: SOURCE_CHECKOUT,
        closeOldPane: params.closeOldPane ?? true,
        copyExtension: params.copyExtension ?? true,
      });
    },
  });

  pi.registerCommand("herdr-worktree-start", {
    description:
      "Create a Herdr worktree from master, continue this pi session in it, and clean up the old pane",
    handler: async (args, ctx) => {
      await ctx.waitForIdle();
      try {
        const parsed = parseCommandArgs(args ?? "");
        const result = await startHerdrWorktree(pi, ctx, undefined, parsed);
        const text = result.content?.[0]?.type === "text" ? result.content[0].text : "Started worktree";
        ctx.ui.notify(text, "info");
      } catch (err: any) {
        ctx.ui.notify(err?.message ?? String(err), "error");
      }
    },
  });
}

async function startHerdrWorktree(
  pi: ExtensionAPI,
  ctx: ExtensionContext,
  signal: AbortSignal | undefined,
  options: StartOptions,
) {
  if (process.env.HERDR_ENV !== "1") {
    throw new Error("herdr_start_worktree must run inside a Herdr-managed pane");
  }

  const oldPaneId = process.env.HERDR_PANE_ID;
  if (options.closeOldPane && !oldPaneId) {
    throw new Error("HERDR_PANE_ID is missing; cannot close the old Herdr pane safely");
  }

  const currentFile = ctx.sessionManager.getSessionFile();
  if (!currentFile) {
    throw new Error("Current pi session is not persisted, so it cannot be continued in a worktree");
  }

  const sourceCheckout = await canonicalDirectory(options.sourceCheckout || SOURCE_CHECKOUT);
  ctx.ui.setStatus("herdr-worktree", "creating worktree");

  let newSessionFile: string | undefined;
  try {
    const created = await createHerdrWorktree(pi, signal, sourceCheckout, options);
    const worktreePath = await canonicalDirectory(created.worktreePath);

    if (options.copyExtension) {
      await copyThisExtension(worktreePath);
    }

    newSessionFile = await forkSessionFile(currentFile, worktreePath);

    await runInNewPane(pi, signal, created.rootPaneId, newSessionFile, worktreePath);

    if (options.closeOldPane && oldPaneId) {
      await scheduleOldPaneCleanup(pi, signal, currentFile, oldPaneId, process.pid);
    }

    ctx.ui.setStatus("herdr-worktree", undefined);
    ctx.ui.notify(`Started pi in Herdr worktree: ${worktreePath}`, "info");
    ctx.shutdown();

    return {
      content: [
        {
          type: "text" as const,
          text:
            `Started replacement pi in Herdr worktree: ${worktreePath}\n` +
            `Workspace: ${created.workspaceId ?? "unknown"}\n` +
            `Pane: ${created.rootPaneId}\n` +
            `Branch: ${created.branch ?? "generated by Herdr"}\n\n` +
            "The old pi process is shutting down. The old pane will close after it exits.",
        },
      ],
      details: {
        worktreePath,
        branch: created.branch,
        workspaceId: created.workspaceId,
        tabId: created.tabId,
        paneId: created.rootPaneId,
        newSessionFile,
        oldSessionFile: currentFile,
        oldPaneId,
      },
      terminate: true,
    };
  } catch (err) {
    ctx.ui.setStatus("herdr-worktree", undefined);
    if (newSessionFile) {
      await rm(newSessionFile, { force: true }).catch(() => undefined);
    }
    throw err;
  }
}

async function createHerdrWorktree(
  pi: ExtensionAPI,
  signal: AbortSignal | undefined,
  sourceCheckout: string,
  options: StartOptions,
): Promise<{
  worktreePath: string;
  branch?: string;
  workspaceId?: string;
  tabId?: string;
  rootPaneId: string;
}> {
  const args = [
    "worktree",
    "create",
    "--cwd",
    sourceCheckout,
    "--base",
    options.base || DEFAULT_BASE,
    "--json",
  ];

  if (options.branch) args.push("--branch", options.branch);
  if (options.label) args.push("--label", options.label);

  const json = await herdrJson(pi, args, signal, sourceCheckout, 120_000);
  const worktreePath = json.result?.worktree?.path;
  const rootPaneId = json.result?.root_pane?.pane_id;
  if (!worktreePath || !rootPaneId) {
    throw new Error("Herdr worktree create response did not include worktree.path and root_pane.pane_id");
  }

  return {
    worktreePath,
    rootPaneId,
    branch: json.result?.worktree?.branch,
    workspaceId: json.result?.workspace?.workspace_id,
    tabId: json.result?.tab?.tab_id,
  };
}

async function runInNewPane(
  pi: ExtensionAPI,
  signal: AbortSignal | undefined,
  paneId: string,
  sessionFile: string,
  worktreePath: string,
): Promise<void> {
  const continuation = `Moved to worktree ${worktreePath}. Continue.`;
  const command = ["pi", "--session", sessionFile, continuation].map(shellQuote).join(" ");
  await herdr(pi, ["pane", "run", paneId, command], signal, undefined, 10_000);
}

async function scheduleOldPaneCleanup(
  pi: ExtensionAPI,
  signal: AbortSignal | undefined,
  oldSessionFile: string,
  oldPaneId: string,
  oldPid: number,
): Promise<void> {
  const cleanup = [
    `old_pid=${oldPid}`,
    `old_session=${shellQuote(oldSessionFile)}`,
    `old_pane=${shellQuote(oldPaneId)}`,
    "i=0",
    "while kill -0 \"$old_pid\" 2>/dev/null && [ \"$i\" -lt 600 ]; do i=$((i + 1)); sleep 0.1; done",
    "rm -f -- \"$old_session\"",
    "herdr pane close \"$old_pane\" >/dev/null 2>&1 || true",
  ].join("; ");

  const launcher =
    "if command -v setsid >/dev/null 2>&1; then " +
    `setsid sh -c ${shellQuote(cleanup)} >/dev/null 2>&1 < /dev/null & ` +
    "else " +
    `nohup sh -c ${shellQuote(cleanup)} >/dev/null 2>&1 < /dev/null & ` +
    "fi";

  const result = await pi.exec("sh", ["-lc", launcher], { signal, timeout: 5_000 });
  if (result.code !== 0) {
    throw new Error(`Failed to schedule old pane cleanup: ${result.stderr || result.stdout}`);
  }
}

async function forkSessionFile(currentFile: string, worktreePath: string): Promise<string> {
  const forked = SessionManager.forkFrom(currentFile, worktreePath);
  const newFile = forked.getSessionFile();
  if (!newFile) {
    throw new Error("Failed to create forked session file for the new worktree");
  }

  const raw = await readFile(newFile, "utf8");
  const lines = raw.trimEnd().split("\n");
  if (lines.length > 0 && lines[0]) {
    const header = JSON.parse(lines[0]);
    if (header.parentSession !== undefined) {
      delete header.parentSession;
      lines[0] = JSON.stringify(header);
      await writeFile(newFile, lines.join("\n") + "\n", "utf8");
    }
  }

  return newFile;
}

async function copyThisExtension(worktreePath: string): Promise<void> {
  const targetDir = join(worktreePath, ".pi", "extensions", EXTENSION_NAME);
  const source = await realpath(EXTENSION_DIR);
  const targetParent = dirname(targetDir);
  await mkdir(targetParent, { recursive: true });
  await cp(source, targetDir, {
    recursive: true,
    force: true,
    filter: (src) => !src.includes(`${EXTENSION_NAME}/node_modules`),
  });
}

async function canonicalDirectory(path: string): Promise<string> {
  const resolved = resolve(path.replace(/^@/, ""));
  const s = await stat(resolved).catch(() => undefined);
  if (!s?.isDirectory()) {
    throw new Error(`Directory does not exist: ${resolved}`);
  }
  return realpath(resolved);
}

async function herdrJson(
  pi: ExtensionAPI,
  args: string[],
  signal: AbortSignal | undefined,
  cwd: string | undefined,
  timeout: number,
): Promise<HerdrWorktreeResult> {
  const result = await herdr(pi, args, signal, cwd, timeout);
  const raw = result.stdout.trim() || result.stderr.trim();
  let json: HerdrWorktreeResult;
  try {
    json = JSON.parse(raw) as HerdrWorktreeResult;
  } catch {
    throw new Error(`Herdr returned non-JSON output for ${args.join(" ")}: ${raw}`);
  }
  if (json.error) {
    throw new Error(`${json.error.code ?? "herdr_error"}: ${json.error.message ?? "unknown Herdr error"}`);
  }
  return json;
}

async function herdr(
  pi: ExtensionAPI,
  args: string[],
  signal: AbortSignal | undefined,
  cwd: string | undefined,
  timeout: number,
) {
  const result = await pi.exec("herdr", args, { cwd, signal, timeout });
  if (result.code !== 0) {
    throw new Error(`herdr ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
  }
  return result;
}

function parseCommandArgs(args: string): StartOptions {
  const tokens = tokenize(args);
  const options: StartOptions = {
    base: DEFAULT_BASE,
    sourceCheckout: SOURCE_CHECKOUT,
    closeOldPane: true,
    copyExtension: true,
  };

  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === "--branch") options.branch = requireValue(tokens, ++i, token);
    else if (token === "--base") options.base = requireValue(tokens, ++i, token);
    else if (token === "--label") options.label = requireValue(tokens, ++i, token);
    else if (token === "--source") options.sourceCheckout = requireValue(tokens, ++i, token);
    else if (token === "--no-close-pane") options.closeOldPane = false;
    else if (token === "--no-copy-extension") options.copyExtension = false;
    else if (!options.branch) options.branch = token;
    else if (!options.label) options.label = token;
    else throw new Error(`Unexpected argument: ${token}`);
  }

  options.branch = cleanOptional(options.branch);
  options.base = cleanOptional(options.base) ?? DEFAULT_BASE;
  options.label = cleanOptional(options.label);
  options.sourceCheckout = cleanOptional(options.sourceCheckout) ?? SOURCE_CHECKOUT;
  return options;
}

function tokenize(input: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let quote: '"' | "'" | undefined;
  let escaping = false;

  for (const ch of input.trim()) {
    if (escaping) {
      current += ch;
      escaping = false;
      continue;
    }
    if (ch === "\\") {
      escaping = true;
      continue;
    }
    if (quote) {
      if (ch === quote) quote = undefined;
      else current += ch;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (/\s/.test(ch)) {
      if (current) {
        tokens.push(current);
        current = "";
      }
      continue;
    }
    current += ch;
  }

  if (quote) throw new Error("Unclosed quote in command arguments");
  if (escaping) current += "\\";
  if (current) tokens.push(current);
  return tokens;
}

function requireValue(tokens: string[], index: number, flag: string): string {
  const value = tokens[index];
  if (!value || value.startsWith("--")) {
    throw new Error(`Missing value for ${flag}`);
  }
  return value;
}

function cleanOptional(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed ? trimmed : undefined;
}

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}
