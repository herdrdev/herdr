#!/usr/bin/env bun
/** Linux-only, real OpenCode / dynamic registry acceptance harness.
 * Run only after compiling the candidate: bun run scripts/agent_registry_opencode_e2e.ts --binary target/debug/herdr
 * Requires an existing Herdr session. Never launches/attaches/stops the default session.
 * No reporters/plugins, report-agent calls, remote models, or global config writes.
 */
import assert from "node:assert/strict";
import path from "node:path";
import { appendFileSync } from "node:fs";
import { access, chmod, cp, mkdir, mkdtemp, readFile, readdir, readlink, rename, writeFile } from "node:fs/promises";
import { Database } from "bun:sqlite";
import { createFixtureProvider, selfTestProvider } from "./agent_registry_opencode_provider";

const repo = path.resolve(import.meta.dir, "..");
const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
const unwrap = (value: any) => value.result ?? value;
const json = (text: string) => JSON.parse(text.trim());
const clearedHerdr = ["HERDR_SOCKET_PATH", "HERDR_CLIENT_SOCKET_PATH", "HERDR_SESSION", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID", "HERDR_PANE_ID"];
type Check = { name: string; status: "pass" | "prerequisite" | "fail"; detail?: unknown };
type Session = { name: string; running: boolean; socket_path: string; session_dir: string };
type TrackedProcess = { pid: number; start: string; executable: string };

async function processStart(pid: number) {
  const stat = await readFile(`/proc/${pid}/stat`, "utf8");
  return stat.slice(stat.lastIndexOf(")") + 2).split(" ")[19];
}
async function signalTracked(process: TrackedProcess, signal: NodeJS.Signals) {
  assert.equal(await processStart(process.pid), process.start, "refusing signal: PID was reused");
  assert.equal(await readlink(`/proc/${process.pid}/exe`), process.executable, "refusing signal: executable changed");
  globalThis.process.kill(process.pid, signal);
}

/** Read only this run's isolated databases. Never inspect global OpenCode storage. */
async function localSessionID(root: string, work: string): Promise<string | undefined> {
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const file = path.join(root, entry.name);
    if (entry.isDirectory()) {
      const id = await localSessionID(file, work);
      if (id) return id;
    } else if (entry.name.endsWith(".db") || entry.name.endsWith(".sqlite")) {
      let db: Database | undefined;
      try {
        db = new Database(file, { readonly: true });
        const columns = db.query("PRAGMA table_info(session)").all() as { name: string }[];
        if (!["id", "directory"].every((name) => columns.some((column) => column.name === name))) continue;
        const row = db.query("SELECT id FROM session WHERE directory = ? ORDER BY rowid DESC LIMIT 1").get(work) as { id?: string } | null;
        if (row?.id?.startsWith("ses_")) return row.id;
      } catch { /* Schema/version varies; lack of a reference is a prerequisite, not a fake pass. */ }
      finally { db?.close(); }
    }
  }
}

async function runHarness(binaryArg: string) {
  assert.equal(process.platform, "linux", "this harness currently supports local Linux only");
  assert.equal(process.env.HERDR_ENV, "1", "requires an existing Herdr session (no CI PTY fallback yet)");
  assert(process.env.HERDR_PANE_ID, "caller pane ID is required for explicit outer-pane split");
  const binary = path.resolve(binaryArg);
  await access(binary);
  const opencode = Bun.which("opencode");
  const parentBinary = process.env.HERDR_BIN_PATH || Bun.which("herdr");
  assert(opencode && parentBinary, "installed opencode and parent Herdr CLI are required");
  // Keep UNIX socket paths below sockaddr_un's 108-byte limit, including named-session suffixes.
  const root = await mkdtemp("/var/tmp/hr-oc-");
  const nonce = path.basename(root).split("-").at(-1)!.toLowerCase();
  const sessionName = `registry-oc-${Date.now().toString(36)}-${nonce}`;
  assert(sessionName !== "default");
  const novel = `opencode-lab-${nonce}`;
  const source = path.join(root, "registry-source");
  const badSource = path.join(root, "bad-source");
  const home = path.join(root, "home");
  const config = path.join(home, ".config/opencode");
  const work = path.join(root, "work");
  const commandsLog = path.join(root, "commands.jsonl");
  const delayedLaunchName = `opencode-delayed-${nonce}`;
  const delayedLaunch = path.join(root, "bin", delayedLaunchName);
  const checks: Check[] = [];
  const provider = createFixtureProvider({ onEvent: (event) => appendFileSync(path.join(root, "provider.jsonl"), `${JSON.stringify(event)}\n`) });
  let outerPane: string | undefined;
  let pane: string | undefined;
  let sessionInfo: Session | undefined;
  let frozen: TrackedProcess | undefined;
  let sessionOwned = false;
  let sessionMayExist = false;
  let failure: unknown;
  let cleaning = false;
  let interrupted = false;
  const interrupt = () => { interrupted = true; };
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", interrupt);
  const check = (name: string, status: Check["status"], detail?: unknown) => {
    checks.push({ name, status, detail });
    console.log(`${status.toUpperCase()}: ${name}`);
  };
  console.log(`Artifacts: ${root}\nSession: ${sessionName}\nNovel agent: ${novel}`);

  // Clean allowlist: no auth, plugins, proxy credentials, experimental flags or parent selection.
  const env: Record<string, string> = {
    PATH: [...new Set([path.join(root, "bin"), path.dirname(opencode), path.dirname(Bun.which("bun") || process.execPath), "/usr/local/bin", "/usr/bin", "/bin"])].join(":"),
    HOME: home, SHELL: "/bin/bash", USER: process.env.USER || "fixture", LOGNAME: process.env.USER || "fixture",
    TERM: process.env.TERM || "xterm-256color", COLORTERM: "truecolor", LANG: "C.UTF-8",
    XDG_CONFIG_HOME: path.join(home, ".config"), XDG_DATA_HOME: path.join(root, "data"),
    XDG_CACHE_HOME: path.join(root, "cache"), XDG_STATE_HOME: path.join(root, "state"), TMPDIR: path.join(root, "tmp"),
    HERDR_ENV: "1", HERDR_SESSION: sessionName, HERDR_CONFIG_PATH: path.join(root, "herdr/config.toml"),
    HERDR_AGENT_REGISTRY_SOURCE: source,
    OPENCODE_TEST_HOME: home, OPENCODE_CONFIG_DIR: config, OPENCODE_CONFIG: path.join(config, "opencode.json"),
    OPENCODE_TUI_CONFIG: path.join(config, "tui.json"), OPENCODE_TEST_MANAGED_CONFIG_DIR: path.join(root, "managed"),
    OPENCODE_AUTH_CONTENT: "{}", OPENCODE_PURE: "1", OPENCODE_DISABLE_PROJECT_CONFIG: "1",
    OPENCODE_DISABLE_DEFAULT_PLUGINS: "1", OPENCODE_DISABLE_EXTERNAL_SKILLS: "1", OPENCODE_DISABLE_CLAUDE_CODE: "1",
    OPENCODE_DISABLE_MODELS_FETCH: "1", OPENCODE_DISABLE_AUTOUPDATE: "1", OPENCODE_DISABLE_AUTOCOMPACT: "1",
    OPENCODE_DISABLE_LSP_DOWNLOAD: "1", OPENCODE_DISABLE_SHARE: "1", OPENCODE_DISABLE_FFF: "1",
    // Best-effort deny background package/update traffic too; no model endpoint can leave loopback.
    HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9", ALL_PROXY: "http://127.0.0.1:9",
    NO_PROXY: "127.0.0.1,localhost", no_proxy: "127.0.0.1,localhost",
  };
  const controlEnv = { ...env };
  for (const key of clearedHerdr) delete controlEnv[key];
  controlEnv.HERDR_SESSION = sessionName;

  async function exec(executable: string, args: string[], environment = controlEnv, allowFailure = false) {
    if (interrupted && !cleaning) throw new Error("interrupted; cleaning up owned resources");
    const started = Date.now();
    const child = Bun.spawn([executable, ...args], { cwd: repo, env: environment, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 25000);
    try {
      const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
      appendFileSync(commandsLog, `${JSON.stringify({ at: new Date(started).toISOString(), ms: Date.now() - started, executable, args, code, timedOut, stdout, stderr })}\n`);
      if (!allowFailure) assert(code === 0 && !timedOut, `${executable} ${args.join(" ")}: ${stderr || stdout}${timedOut ? " (timeout)" : ""}`);
      return { stdout, stderr, code, timedOut };
    } finally { clearTimeout(timer); }
  }
  // Several successful mutation commands (notably pane run/send-text) deliberately print nothing.
  const response = (stdout: string) => stdout.trim() ? unwrap(json(stdout)) : {};
  const cli = async (args: string[]) => response((await exec(binary, args)).stdout);
  const parent = async (args: string[]) => response((await exec(parentBinary!, args, process.env as Record<string, string>)).stdout);
  async function rawApi(request: unknown) {
    assert(sessionInfo?.socket_path, "named session socket must come from session list");
    const code = "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2]+'\\n').encode()); print(s.makefile('r').readline(), end='')";
    return json((await exec("python3", ["-c", code, sessionInfo.socket_path, JSON.stringify(request)])).stdout);
  }
  const sessions = async (): Promise<Session[]> => json((await exec(binary, ["session", "list", "--json"])).stdout).sessions;
  async function poll<T>(name: string, fn: () => Promise<T | undefined | false>, timeout = 20000): Promise<T> {
    const deadline = Date.now() + timeout;
    let last: unknown;
    while (Date.now() < deadline) {
      if (interrupted && !cleaning) throw new Error("interrupted");
      try { const value = await fn(); if (value !== undefined && value !== false) return value as T; }
      catch (error) { last = error; }
      await Bun.sleep(150);
    }
    throw new Error(`timeout: ${name}${last ? `; last error: ${last}` : ""}`);
  }
  async function state(status: string) {
    assert(pane);
    return poll(`agent ${status}`, async () => {
      const info = (await cli(["agent", "get", pane!])).agent;
      return info.agent === novel && info.agent_status === status ? info : undefined;
    }, 30000);
  }
  async function productReady(history = false) {
    const ready = await poll("product interactive_ready", async () => {
      const info = (await cli(["agent", "get", pane!])).agent;
      return info.interactive_ready === true && !info.launch_pending ? info : undefined;
    }, 60000);
    const screen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
    assert(/^\s*╹▀{8,}\s*$/m.test(screen), "interactive_ready must correspond to the painted composer frame");
    assert(/commands/i.test(screen), "interactive_ready must correspond to a visible commands action");
    if (history) assert(screen.includes("Fixture complete."), "resumed interactive_ready must include restored conversation history");
    return { ready, screen };
  }
  async function rejectPrematurePrompt(label: string) {
    const startup = await poll("startup visible before interactive_ready", async () => {
      const info = (await cli(["agent", "get", pane!])).agent;
      if (info.interactive_ready || !info.launch_pending) return;
      const screen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
      return screen.trim() ? { info, screen } : undefined;
    }, 30000);
    const marker = `HERDR_E2E_PREMATURE_${nonce}_${label}`;
    const providerCallsBefore = provider.events.filter((event) => event.type === "request").length;
    const rejected = await exec(binary, ["agent", "prompt", pane!, marker], controlEnv, true);
    assert(rejected.code !== 0 && !rejected.timedOut, "premature prompt must be rejected synchronously");
    const payload = json(rejected.stderr || rejected.stdout);
    assert.equal(payload.error?.code, "agent_not_ready");
    await Bun.sleep(350);
    const after = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
    assert(!after.includes(marker), "rejected premature prompt must not write to the PTY");
    assert.equal(provider.events.filter((event) => event.type === "request").length, providerCallsBefore, "rejected premature prompt must not call the provider");
    await writeFile(path.join(root, `${label}-premature-rejection.json`), JSON.stringify({ startup: startup.info, screen: startup.screen, response: payload, providerCallsBefore }, null, 2));
    check(`${label} rejects prompt before product readiness`, "pass");
  }
  async function capture(label: string, recognized = true) {
    assert(pane);
    await writeFile(path.join(root, `${label}.pane.json`), JSON.stringify(await cli(["pane", "get", pane]), null, 2));
    await writeFile(path.join(root, `${label}.process.json`), JSON.stringify(await cli(["pane", "process-info", "--pane", pane]), null, 2));
    if (recognized) {
      await writeFile(path.join(root, `${label}.agent.json`), JSON.stringify(await cli(["agent", "get", pane]), null, 2));
      const explain = await exec(binary, ["agent", "explain", pane, "--json"], controlEnv, true);
      await writeFile(path.join(root, `${label}.explain.json`), explain.stdout || explain.stderr);
    }
    for (const format of ["text", "ansi"]) {
      const result = await exec(binary, [recognized ? "agent" : "pane", "read", pane, "--source", recognized ? "detection" : "recent", "--format", format]);
      await writeFile(path.join(root, `${label}.${format === "text" ? "txt" : "ansi"}`), result.stdout);
    }
  }
  async function foreground(): Promise<TrackedProcess | undefined> {
    const info = (await cli(["pane", "process-info", "--pane", pane!])).process_info;
    for (const item of info.foreground_processes ?? []) {
      try {
        const executable = await readlink(`/proc/${item.pid}/exe`);
        if (executable === await readlinkResolved(opencode!)) return { pid: item.pid, executable, start: await processStart(item.pid) };
      } catch { /* A process may exit between API and /proc reads. */ }
    }
  }
  async function readlinkResolved(file: string) {
    // realpath supports both ELF files and symlink-based installations.
    return (await import("node:fs/promises")).realpath(file);
  }
  async function namedLaunch(round: string) {
    assert(outerPane);
    sessionMayExist = true;
    const launchEnv = { ...env };
    for (const key of clearedHerdr) delete launchEnv[key];
    const command = `env -i ${Object.entries(launchEnv).map(([key, value]) => `${key}=${quote(value)}`).join(" ")} ${quote(binary)} --session ${quote(sessionName)}; printf ${quote(`\\nHERDR_E2E_NAMED_EXIT_${nonce}_${round}\\n`)}`;
    await parent(["pane", "run", outerPane, command]);
    await poll("named session API", async () => {
      const list = await sessions();
      const item = list.find((item) => item.name === sessionName && item.running);
      if (!item) return;
      sessionInfo = item; // IDs and socket/directory paths originate in command output.
      const panes = (await cli(["pane", "list"])).panes;
      if (!panes?.length) return;
      sessionOwned = true;
      pane = panes[0].pane_id;
      return true;
    }, 45000);
    await writeFile(path.join(root, `session-${round}.json`), JSON.stringify(sessionInfo, null, 2));
  }
  async function namedStop(round: string) {
    assert(sessionOwned && sessionInfo?.name === sessionName && outerPane);
    await exec(binary, ["session", "stop", sessionName, "--json"]);
    await poll("named session stopped", async () => !(await sessions()).some((item) => item.name === sessionName && item.running));
    await exec(parentBinary!, ["pane", "wait-output", outerPane, "--match", `HERDR_E2E_NAMED_EXIT_${nonce}_${round}`, "--timeout", "20000"], process.env as Record<string, string>);
  }
  async function exitOpenCode(label: string) {
    assert(pane && await foreground(), "refusing exit input: target is not tracked OpenCode");
    await cli(["pane", "send-text", pane, "/exit"]);
    await cli(["pane", "send-keys", pane, "Enter"]);
    await poll("OpenCode exits", async () => !(await foreground()));
    await poll("agent clears after exit", async () => !(await cli(["pane", "get", pane!])).pane.agent);
    await capture(label, false);
  }
  async function turn(mode: "COMPLETE" | "PERMISSION" | "CANCEL", suffix: string) {
    const marker = `HERDR_E2E_${mode}_${nonce}_${suffix}`;
    await cli(["agent", "prompt", pane!, marker]);
    await poll(`provider gate ${mode}`, async () => provider.events.some((event) => event.type === "gated" && event.marker === marker));
    await state("working");
    await capture(`${suffix}-working`);
    if (mode === "CANCEL") {
      await cli(["pane", "send-keys", pane!, "Escape"]);
      await cli(["pane", "send-keys", pane!, "Escape"]);
      await poll("HTTP stream aborted", async () => provider.events.some((event) => event.type === "aborted" && event.marker === marker));
      assert(!provider.events.some((event) => event.type === "finished" && event.marker === marker), "cancelled turn must not complete");
    } else {
      await provider.release(marker);
      if (mode === "PERMISSION") {
        await state("blocked");
        await capture(`${suffix}-blocked`);
        const screen = await readFile(path.join(root, `${suffix}-blocked.txt`), "utf8");
        assert(screen.includes("Permission required"), "must be real permission UI, not just a blocked state");
        await cli(["pane", "send-keys", pane!, "Escape"]); // Root permission Reject, never approve.
      }
    }
    await state("idle");
    await capture(`${suffix}-idle`);
    check(`real UI ${mode.toLowerCase()} → idle`, "pass");
  }

  try {
    for (const dir of [config, work, path.dirname(delayedLaunch), env.XDG_DATA_HOME, env.XDG_CACHE_HOME, env.XDG_STATE_HOME, env.TMPDIR, env.OPENCODE_TEST_MANAGED_CONFIG_DIR, path.dirname(env.HERDR_CONFIG_PATH), path.join(source, "agents")]) await mkdir(dir, { recursive: true });
    await writeFile(delayedLaunch, `#!/bin/sh\nsleep 1.5\nexec ${quote(opencode)} "$@"\n`);
    await chmod(delayedLaunch, 0o700);
    await writeFile(env.HERDR_CONFIG_PATH, 'onboarding = false\n[terminal]\ndefault_shell = "/bin/bash"\nshell_mode = "non_login"\n[session]\nresume_agents_on_restore = true\n[experimental]\nallow_nested = true\n[update]\nversion_check = false\nmanifest_check = false\n');
    await writeFile(path.join(config, "tui.json"), JSON.stringify({ plugin: [] }));
    await writeFile(path.join(config, "opencode.json"), JSON.stringify({
      model: "herdr-local/fixture", small_model: "herdr-local/fixture", enabled_providers: ["herdr-local"],
      provider: { "herdr-local": { npm: "@ai-sdk/openai-compatible", name: "Herdr Local Fixture", env: [],
        options: { baseURL: `${provider.url}/v1`, apiKey: "local-not-a-secret" },
        models: { fixture: { name: "Herdr Fixture", tool_call: true, reasoning: false, limit: { context: 32768, output: 1024 }, cost: { input: 0, output: 0 } } } } },
      permission: { "*": "deny", bash: "ask" }, agent: { title: { disable: true }, summary: { disable: true } },
      share: "disabled", autoupdate: false, snapshot: false, lsp: false, formatter: false,
    }, null, 2));
    await writeFile(path.join(root, "launch-environment.json"), JSON.stringify(env, null, 2));
    const agents = path.join(repo, "vendor/agent-registry/agents");
    for (const entry of await readdir(agents)) if (entry !== "opencode") await cp(path.join(agents, entry), path.join(source, "agents", entry), { recursive: true });

    // Installed/candidate help is the authority. These calls are noninteractive.
    for (const [exe, args, environment] of [
      [binary, ["--version"], controlEnv], [binary, ["registry", "--help"], controlEnv],
      [binary, ["pane", "--help"], controlEnv], [binary, ["agent", "--help"], controlEnv],
      [binary, ["session", "--help"], controlEnv], [opencode, ["--version"], controlEnv],
      [opencode, ["--help"], controlEnv], [parentBinary, ["pane", "split", "--help"], process.env],
    ] as [string, string[], Record<string, string>][]) await exec(exe, args, environment);
    await exec(binary, ["registry", "validate", source]);
    assert(!(await sessions()).some((item) => item.name === sessionName), "refusing to reuse existing session");
    const parentBefore = await parent(["pane", "get", process.env.HERDR_PANE_ID!]);
    await writeFile(path.join(root, "parent-before.json"), JSON.stringify(parentBefore, null, 2));
    const split = await parent(["pane", "split", process.env.HERDR_PANE_ID!, "--direction", "down", "--ratio", "0.3", "--cwd", root, "--no-focus"]);
    outerPane = split.pane?.pane_id;
    assert(outerPane && outerPane !== process.env.HERDR_PANE_ID, "missing/unsafe outer pane ID");
    await namedLaunch("first");
    const initial = (await cli(["registry", "status"])).registry;
    assert.equal(initial.source, source);
    assert(!initial.agents.some((agent: any) => agent.id === "opencode" || agent.id === novel));
    await writeFile(path.join(root, "registry-initial.json"), JSON.stringify(initial, null, 2));
    const rootPane = (await cli(["pane", "get", pane!])).pane;
    const rootProcess = (await cli(["pane", "process-info", "--pane", pane!])).process_info;
    assert.equal(rootPane.agent, undefined);
    assert(rootPane.cwd && rootProcess.shell_pid, "new disposable root must have a known cwd/shell");
    assert((rootProcess.foreground_processes ?? []).every((item: any) => item.pid === rootProcess.shell_pid), "disposable root is not at its shell");
    await writeFile(path.join(root, "disposable-root-before-launch.json"), JSON.stringify({ pane: rootPane, process: rootProcess }, null, 2));
    // Keep an intermediate noninteractive shell alive while SIGSTOP freezes its OpenCode child.
    // Stopping a direct child of the interactive shell would otherwise return its job to the shell.
    const ocCommand = `${quote(opencode)} --model herdr-local/fixture --agent build; printf ${quote(`\\nHERDR_E2E_OC_EXIT_${nonce}\\n`)}`;
    await cli(["pane", "run", pane!, `cd ${quote(work)} && /bin/bash --noprofile --norc -c ${quote(ocCommand)}`]);
    const trackedOpenCode = await poll("real OpenCode foreground", foreground, 30000);
    await poll("OpenCode home screen", async () => {
      const screen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
      return /Herdr Fixture|herdr-local|fixture/i.test(screen) ? true : undefined;
    }, 60000);
    assert(!(await cli(["pane", "get", pane!])).pane.agent, "OpenCode must initially be unrecognized");
    await capture("initial-unknown", false);
    check("running OpenCode initially unknown", "pass");

    // Stop the exact API-observed executable, not a name match or guessed PID. No input during reload.
    frozen = trackedOpenCode;
    await signalTracked(frozen, "SIGSTOP");
    let previous = "";
    let stable = 0;
    const frozenScreen = await poll("drain pre-SIGSTOP screen", async () => {
      const screen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
      stable = screen === previous ? stable + 1 : 0;
      previous = screen;
      return stable >= 3 ? { screen } : undefined;
    });
    const novelDir = path.join(source, "agents", novel);
    await mkdir(novelDir);
    for (const name of ["agent.toml", "process.toml", "detection.toml", "resume.toml"]) {
      let content = await readFile(path.join(agents, "opencode", name), "utf8");
      if (name === "agent.toml") {
        content = content.replace(/^id = "opencode"$/m, `id = "${novel}"`).replace(/^name = "opencode"$/m, `name = "${novel}"`).replace(/^aliases = .*$/m, 'aliases = []');
        content = content.replace(/^unix = "opencode"$/m, `unix = "${delayedLaunchName}"`);
        content = content.replace(/\n\[sound\][\s\S]*$/, "\n");
      }
      if (name === "detection.toml") content = content.replace(/^id = "opencode"$/m, `id = "${novel}"`).replace(/^aliases = .*$/m, 'aliases = []');
      // process.toml retains actual installed opencode process names. No integration.toml/assets.
      await writeFile(path.join(novelDir, name), content);
    }
    await exec(binary, ["registry", "validate", source]);
    let activated = (await cli(["registry", "reload", source])).registry;
    assert(activated.generation > initial.generation && activated.digest !== initial.digest);
    const summary = activated.agents.find((agent: any) => agent.id === novel);
    assert(summary?.process && summary?.detection && summary?.resume && !summary?.integration);
    await poll("same frozen process recognized after reload", async () => (await cli(["pane", "get", pane!])).pane.agent === novel);
    const afterScreen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
    assert.equal(afterScreen, frozenScreen.screen, "reload must not change the pane screen");
    await capture("reloaded-frozen");
    check("novel registry detects existing stopped OpenCode without input", "pass", { process: trackedOpenCode, screenUnchanged: true, byteCounterAvailable: false });
    await signalTracked(frozen, "SIGCONT");
    frozen = undefined;
    await state("idle");
    await capture("recognized-idle");
    await writeFile(path.join(root, "recognized-idle.layout.json"), JSON.stringify(await cli(["pane", "layout"]), null, 2));
    const idleScreen = await readFile(path.join(root, "recognized-idle.txt"), "utf8");
    await cli(["pane", "send-keys", pane!, "Ctrl+p"]);
    await poll("OpenCode command palette paints", async () => {
      const screen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
      return screen !== idleScreen && /command/i.test(screen) ? true : undefined;
    });
    await capture("command-palette");
    const paletteExplain = json((await exec(binary, ["agent", "explain", pane!, "--json"])).stdout);
    assert.equal(paletteExplain.state, "unknown");
    assert.equal(paletteExplain.skip_state_update, true);
    assert.equal(paletteExplain.matched_rule?.id, "command_palette");
    await cli(["pane", "send-keys", pane!, "Escape"]);
    await poll("OpenCode command palette closes", async () => {
      const screen = (await exec(binary, ["pane", "read", pane!, "--source", "recent", "--format", "text"])).stdout;
      return !/select a command|search commands/i.test(screen) ? true : undefined;
    });
    await capture("recognized-idle-after-command-palette");
    // Publication returned only after journal persistence. Copy the committed evidence now.
    assert(sessionInfo);
    const journalPath = path.join(sessionInfo.session_dir, "agent-registry/active.json");
    const journal = json(await readFile(journalPath, "utf8"));
    assert.equal(journal.generation, activated.generation);
    assert.equal(journal.digest, activated.digest);
    await cp(journalPath, path.join(root, "committed-registry.json"));
    check("published registry already persisted", "pass");

    await mkdir(path.join(badSource, "agents", novel), { recursive: true });
    await writeFile(path.join(badSource, "agents", novel, "agent.toml"), "not valid TOML [[[\n");
    const bad = await exec(binary, ["registry", "reload", badSource], controlEnv, true);
    assert(bad.code !== 0 && !bad.timedOut, "bad reload must be rejected, not time out");
    const retained = (await cli(["registry", "status"])).registry;
    assert.equal(retained.generation, activated.generation);
    assert.equal(retained.digest, activated.digest);
    assert(retained.last_error, "bad reload should be visible in status");
    assert.equal(json(await readFile(journalPath, "utf8")).digest, activated.digest);
    check("bad reload retains active generation/digest and journal", "pass");

    const originalProcess = await foreground();
    assert(originalProcess);
    const retiredPackage = path.join(root, "retired-package");
    await rename(novelDir, retiredPackage);
    const retired = (await cli(["registry", "reload", source])).registry;
    assert(!retired.agents.some((agent: any) => agent.id === novel));
    const denied = await exec(binary, ["agent", "start", "retired-start", "--kind", novel, "--pane", pane!], controlEnv, true);
    assert(denied.code !== 0 && !denied.timedOut);
    assert.equal(json(denied.stderr || denied.stdout).error?.code, "unsupported_agent_kind");
    await turn("COMPLETE", "retained-process");
    assert.deepEqual(await foreground(), originalProcess, "removal must retain the same live process");
    await rename(retiredPackage, novelDir);
    const readded = (await cli(["registry", "reload", source])).registry;
    assert.equal(readded.digest, activated.digest);
    assert(readded.generation > retired.generation);
    activated = readded;
    check("removed package blocks new starts but retains live prompting and identity", "pass");

    await turn("COMPLETE", "complete");
    await turn("PERMISSION", "permission");
    await turn("CANCEL", "cancel");
    await turn("COMPLETE", "after-cancel");
    await exitOpenCode("exited");
    check("real OpenCode exit clears identity", "pass");
    const nativeID = await localSessionID(env.XDG_DATA_HOME, work);
    let beforeNativeRestart: TrackedProcess | undefined;
    const resumeOptions = ["--model", "herdr-local/fixture", "--agent", "build"];
    if (nativeID) {
      await writeFile(path.join(root, "native-session-reference.json"), JSON.stringify({ id: nativeID, source: "isolated OpenCode sqlite (read-only)" }, null, 2));
      // Legitimate internal launch path only; never forge a reporter/source or grant hook authority.
      const started = await rawApi({ id: "e2e:native-explicit-start", method: "agent.start", params: {
        name: "registry-native-resume", kind: novel, pane_id: pane!, args: ["--session", nativeID, ...resumeOptions], timeout_ms: 20000,
      } });
      assert.equal(started.result?.type, "agent_started");
      assert.equal(started.result?.agent?.launch_pending, true);
      assert.notEqual(started.result?.agent?.interactive_ready, true);
      await rejectPrematurePrompt("native-explicit-resume");
      await productReady(true);
      await state("idle");
      const info = await poll("legitimate native reference capture", async () => {
        const info = (await cli(["agent", "get", pane!])).agent;
        return info.agent_session?.value === nativeID && info.agent_session?.agent === novel ? info : undefined;
      });
      assert.equal(info.agent_session.source, "herdr:launch", "native reference must come from internal launch capture, not a reporter");
      beforeNativeRestart = await poll("explicit native OpenCode process", foreground);
      await capture("native-explicit-resume");
      check("explicit native resume launch/reference capture", "pass", { requestedSessionID: nativeID, observedSession: info.agent_session });
      // Leave this idle native session running: stopping Herdr now must persist its pinned recipe.
      // Exiting OpenCode first would test only registry restart, not native restoration.
    } else check("native resume/restore", "prerequisite", "No supported native ID in isolated OpenCode database; no synthetic session report submitted.");

    // Remove only our source via rename, retaining it as evidence; restart the same recorded session.
    await namedStop("first");
    if (nativeID) {
      const savedPath = path.join(sessionInfo!.session_dir, "session.json");
      const saved = json(await readFile(savedPath, "utf8"));
      const references: any[] = [];
      const collect = (value: any) => {
        if (!value || typeof value !== "object") return;
        if (value.agent_session?.agent === novel && value.agent_session?.value === nativeID) references.push(value.agent_session);
        for (const child of Object.values(value)) collect(child);
      };
      collect(saved);
      assert(references.some((ref) => ref.source === "herdr:launch" && ref.recipe), "stopped session must persist legitimate native reference plus pinned recipe");
      for (const ref of references) assert.deepEqual(ref.resume_options, resumeOptions, "persist options only with the exact accepted native session");
      await cp(savedPath, path.join(root, "native-before-restart.session.json"));
      check("native reference and pinned recipe persisted", "pass", references);
    }
    await rename(source, `${source}-removed`);
    await namedLaunch("lkg");
    const restored = (await cli(["registry", "status"])).registry;
    // App startup rebuilds detection overlays via replace_detection, advancing the generation.
    // LKG guarantees package bytes, not an unchanged in-memory detection epoch across processes.
    assert(restored.generation >= activated.generation, "restart must not regress the published generation");
    assert.equal(restored.digest, activated.digest);
    const restoredJournal = json(await readFile(path.join(sessionInfo!.session_dir, "agent-registry/active.json"), "utf8"));
    assert.deepEqual(restoredJournal.files, journal.files, "LKG must retain exact package bytes without the source");
    assert.equal(restoredJournal.generation, restored.generation);
    assert(restored.agents.some((agent: any) => agent.id === novel));
    await writeFile(path.join(root, "registry-lkg.json"), JSON.stringify(restored, null, 2));
    check("restart with missing source reuses persisted LKG", "pass", { beforeGeneration: activated.generation, afterGeneration: restored.generation, packageBytesUnchanged: true, note: "Startup detection rebuild may advance generation" });
    if (nativeID) {
      const resumedProcess = await poll("automatic native OpenCode restore", foreground, 60000);
      assert(beforeNativeRestart && (resumedProcess.pid !== beforeNativeRestart.pid || resumedProcess.start !== beforeNativeRestart.start), "must observe a new OpenCode process after restart");
      const argv = (await readFile(`/proc/${resumedProcess.pid}/cmdline`, "utf8")).split("\0").filter(Boolean);
      assert.deepEqual(argv.slice(1), ["--session", nativeID, ...resumeOptions], "automatic restore must preserve the pinned native session and allowed original CLI choices");
      await capture("native-restored-process-before-ui");
      await rejectPrematurePrompt("native-automatic-restore");
      await productReady(true);
      await state("idle");
      const info = (await cli(["agent", "get", pane!])).agent;
      assert.equal(info.agent_session?.value, nativeID);
      assert.equal(info.agent_session?.agent, novel);
      assert.equal(info.agent_session?.source, "herdr:launch");
      await capture("native-automatic-restore");
      check("automatic native restore from LKG with same session ID", "pass", { before: beforeNativeRestart, after: resumedProcess, argv, observedSession: info.agent_session });
      await turn("COMPLETE", "after-native-restore");
      await exitOpenCode("native-restored-exited");
    }
    assert(!provider.events.some((event) => ["stream_error", "invalid_request", "unexpected_endpoint", "duplicate_turn"].includes(event.type)), "provider contract errors; inspect provider.jsonl");
    check("model requests handled only by loopback fixture", "pass", { requests: provider.events.filter((event) => event.type === "request").length, model: "herdr-local/fixture", externalModelClients: 0 });
  } catch (error) {
    failure = error;
    check("acceptance", "fail", String(error));
    if (outerPane) {
      try {
        const screen = await exec(parentBinary!, ["pane", "read", outerPane, "--source", "recent", "--format", "text"], process.env as Record<string, string>, true);
        await writeFile(path.join(root, "failure-outer.txt"), screen.stdout || screen.stderr);
      } catch { /* Preserve original error. */ }
    }
    if (pane && sessionOwned) {
      try { await capture("failure", false); } catch { /* Preserve original error. */ }
    }
  } finally {
    cleaning = true;
    const cleanupErrors: string[] = [];
    if (frozen) {
      try { await signalTracked(frozen, "SIGCONT"); } catch (error) { cleanupErrors.push(`resume owned OpenCode: ${error}`); }
    }
    // Stop only the unique name checked absent before launch. Re-query for partial startup failures.
    let stopped = !sessionMayExist;
    if (sessionMayExist) {
      try {
        const item = (await sessions()).find((item) => item.name === sessionName);
        if (item) {
          sessionInfo = item;
          if (item.running) await exec(binary, ["session", "stop", sessionName, "--json"]);
          await poll("cleanup session stopped", async () => !(await sessions()).some((entry) => entry.name === sessionName && entry.running));
          // Preserve logs before deleting just this session directory through the official CLI.
          await cp(item.session_dir, path.join(root, "stopped-session"), { recursive: true }).catch((error) => cleanupErrors.push(`copy logs: ${error}`));
          await exec(binary, ["session", "delete", sessionName, "--json"]);
        }
        assert(!(await sessions()).some((item) => item.name === sessionName));
        stopped = true;
      } catch (error) { cleanupErrors.push(`named session cleanup: ${error}`); }
    }
    if (outerPane && stopped) {
      try {
        // Wait until its actual foreground job has returned to the original shell before closing.
        await poll("outer pane returned to shell", async () => {
          const info = (await parent(["pane", "process-info", "--pane", outerPane!])).process_info;
          return info.shell_pid && info.foreground_process_group_id === info.shell_pid && (info.foreground_processes ?? []).every((item: any) => item.pid === info.shell_pid) ? true : undefined;
        });
        await parent(["pane", "close", outerPane]);
      } catch (error) { cleanupErrors.push(`outer pane cleanup (left intact for inspection): ${error}`); }
    } else if (outerPane) cleanupErrors.push(`outer pane ${outerPane} intentionally retained because named stop was not confirmed`);
    await provider.stop();
    check("cleanup", cleanupErrors.length ? "fail" : "pass", cleanupErrors);
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", interrupt);
    const report = { at: new Date().toISOString(), binary, opencode, root, sessionName, novel, outerPane, pane, sessionInfo, checks,
      limitations: ["Linux only; requires HERDR_ENV=1", "No raw PTY byte counter: SIGSTOP plus unchanged screen evidence", "Native launch/automatic restore run only when an isolated native session ID is available; otherwise explicitly flagged prerequisite", "Loopback model allowlist is not an OS network namespace"] };
    await writeFile(path.join(root, "report.json"), JSON.stringify(report, null, 2));
    await mkdir(path.join(repo, ".local/prd"), { recursive: true });
    appendFileSync(path.join(repo, ".local/prd/registry-dynamic-e2e.md"), `\n## Run ${report.at}\n\nArtifacts: \`${root}\`\nCandidate: \`${binary}\`\nSession: \`${sessionName}\`\n\n${checks.map((item) => `- **${item.status}** ${item.name}${item.detail ? ` — ${JSON.stringify(item.detail)}` : ""}`).join("\n")}\n`);
    console.log(`Report: ${root}/report.json`);
    if (failure || cleanupErrors.length) throw failure || new Error(cleanupErrors.join("\n"));
  }
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === "--self-test") return selfTestProvider();
  if (args.length === 1 && args[0] === "--provider-only") {
    const provider = createFixtureProvider({ onEvent: (event) => console.log(JSON.stringify(event)) });
    console.log(`Provider: ${provider.url}/v1; GET /control/status; POST /control/release {"marker":"..."}`);
    await new Promise<void>((resolve) => {
      process.once("SIGINT", resolve);
      process.once("SIGTERM", resolve);
    });
    await provider.stop();
    return;
  }
  if (args.length === 2 && args[0] === "--binary") return runHarness(args[1]);
  console.log("Usage: bun run scripts/agent_registry_opencode_e2e.ts --binary target/debug/herdr\n       bun run scripts/agent_registry_opencode_e2e.ts --self-test | --provider-only\nRequires local Linux inside Herdr; compile candidate first. Leaves evidence under /var/tmp, cleans only owned runtime resources.");
  if (args.length && !args.includes("--help")) process.exitCode = 2;
}
if (import.meta.main) main().catch((error) => { console.error(error); process.exitCode = 1; });
