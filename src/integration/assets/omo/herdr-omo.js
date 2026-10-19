// Omo lifecycle integration for Herdr. Installed as herdr-omo.js.
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";

const SOURCE = "custom:omo";
const sequenceKey = Symbol.for("omo-herdr.report-sequence");

export function agentDirectory(env = process.env, cwd = process.cwd()) {
  for (const name of ["OMO_CODING_AGENT_DIR", "SENPI_CODING_AGENT_DIR", "PI_CODING_AGENT_DIR"]) {
    if (env[name]?.trim()) return resolve(cwd, env[name].trim());
  }
  return join(env.HOME || env.USERPROFILE || homedir(), ".omo", "agent");
}

export function resumeArguments(ctx, pi, env = process.env) {
  const file = ctx.sessionManager.getSessionFile();
  if (!file || !isAbsolute(file) || !existsSync(file)) return undefined;
  const argv = ["env", `OMO_CODING_AGENT_DIR=${agentDirectory(env)}`, "omo", "--session", file];
  if (ctx.model) argv.push("--model", `${ctx.model.provider}/${ctx.model.id}`);
  if (ctx.thinkingLevel) argv.push("--thinking", ctx.thinkingLevel);
  const preset = pi.getPermissionPreset();
  if (preset) argv.push("--permission-preset", preset);
  if (
    argv.length > 64 ||
    argv.some(arg => /['\p{Cc}]/u.test(arg)) ||
    argv.reduce((bytes, arg) => bytes + Buffer.byteLength(arg), 0) > 8192
  ) return undefined;
  return argv;
}

// Each command is bounded. A new state replaces a queued state rather than
// building a backlog behind a disconnected Herdr server.
export function createReporter(env, execute = execFile, now = Date.now) {
  const queued = new Map();
  let draining;
  let closed = false;
  let active;
  let resetPending = false;

  function send(command, args) {
    globalThis[sequenceKey] = Math.max((globalThis[sequenceKey] ?? 0) + 1, now() * 1000);
    const separator = args.indexOf("--");
    const options = separator === -1 ? args : args.slice(0, separator);
    const resume = separator === -1 ? [] : args.slice(separator);
    const argv = [
      "pane", command, env.HERDR_PANE_ID,
      "--source", SOURCE,
      ...options,
      "--seq", String(globalThis[sequenceKey]),
      ...resume,
    ];
    return new Promise(resolveSend => {
      active = new AbortController();
      execute(env.HERDR_BIN_PATH, argv, {
        env,
        timeout: 750,
        maxBuffer: 65536,
        signal: active.signal,
      }, error => {
        active = undefined;
        resolveSend(!error);
      });
    });
  }

  function flush() {
    if (draining) return draining;
    draining = (async () => {
      while (queued.size) {
        const [key, item] = queued.entries().next().value;
        queued.delete(key);
        if (item.command === "report-agent" && resetPending) {
          // Custom reports have no native session identity with which Herdr
          // could invalidate an older session's restore command.
          const released = await send("release-agent", ["--agent", "omo"]);
          if (closed) break;
          if (!released) continue;
          resetPending = false;
        }
        await send(item.command, item.args);
      }
    })().finally(() => {
      draining = undefined;
      if (queued.size) return flush();
    });
    return draining;
  }

  return {
    report(key, command, args, resetResume = false) {
      if (closed) return;
      if (resetResume) resetPending = true;
      queued.set(key, { command, args });
      void flush();
    },
    flush,
    async close(release, signal) {
      closed = true;
      queued.clear();
      const abort = () => active?.abort();
      signal?.addEventListener("abort", abort, { once: true });
      if (signal?.aborted) abort();
      try {
        await draining;
        if (release && !signal?.aborted) {
          await send("report-metadata", [
            "--agent", "omo", "--clear-token", "omo-session",
            "--clear-token", "omo-mark", "--clear-token", "omo-name", "--clear-title",
          ]);
          if (!signal?.aborted) await send("release-agent", ["--agent", "omo"]);
        }
      } finally {
        signal?.removeEventListener("abort", abort);
      }
    },
  };
}

export function createIntegration(pi, {
  env = process.env,
  reporter = createReporter(env),
} = {}) {
  if (
    env.HERDR_ENV !== "1" ||
    !env.HERDR_BIN_PATH ||
    !env.HERDR_SOCKET_PATH ||
    !env.HERDR_PANE_ID
  ) return;

  let owner;
  let context;
  let stopped = false;
  let active = false;
  let compacting = false;
  let monitors = 0;
  let prompt;
  let resumable;
  const blocked = new Map();
  const wakeSources = new Map();
  const subscriptions = [];

  function publish() {
    if (!owner || stopped) return;
    const counts = [];
    for (const [source, count] of [...wakeSources].sort(([a], [b]) => a.localeCompare(b))) {
      if (source === "ask-user" || source === "terminal-monitors") continue;
      const label = {
        "senpi-task": "subagents",
        "omo-dag": "DAG runs",
        "senpi-codemode": "eval cells",
        "terminal-background-sessions": "background sessions",
      }[source] ?? source;
      counts.push(`${count} ${label}`);
    }
    if (monitors) counts.push(`${monitors} monitors`);
    const state = blocked.size || prompt ? "blocked" : active || compacting || counts.length ? "working" : "idle";
    const message = blocked.size ? blocked.values().next().value : prompt || counts.join("; ");
    const args = ["--agent", "omo", "--state", state];
    if (message) args.push("--message", message);
    const resume = resumeArguments(context, pi, env);
    if (resume) args.push("--", ...resume);
    const resetResume = !resume && resumable !== false;
    resumable = Boolean(resume);
    reporter.report("state", "report-agent", args, resetResume);
  }

  function metadata() {
    if (!owner || stopped) return;
    const id = owner.getSessionId();
    const name = owner.getSessionName() ?? "";
    const args = ["--agent", "omo"];
    if (name) args.push("--title", name);
    else args.push("--clear-title");
    args.push("--token", `omo-session=${id}`);
    args.push("--token", "omo-mark=\u25c9 Omo");
    if (name) args.push("--token", `omo-name=${name}`);
    else args.push("--clear-token", "omo-name");
    reporter.report("metadata", "report-metadata", args);
  }

  // Retained monitor/background snapshots are emitted by built-in
  // session_start handlers before user session_start handlers run.
  subscriptions.push(pi.events.on("herdr:blocked", data => {
    if (
      stopped || !data || typeof data.id !== "string" || !data.id ||
      typeof data.active !== "boolean"
    ) return;
    if (data.active) blocked.set(data.id, typeof data.label === "string" ? data.label : "Waiting for input");
    else blocked.delete(data.id);
    publish();
  }));
  subscriptions.push(pi.events.on("terminal_monitor_state", data => {
    if (stopped || !Number.isInteger(data?.activeCount) || data.activeCount < 0) return;
    monitors = data.activeCount;
    publish();
  }));
  subscriptions.push(pi.events.on("wake_source_state", data => {
    if (
      stopped || typeof data?.source !== "string" || !data.source ||
      !Number.isInteger(data.activeCount) || data.activeCount < 0
    ) return;
    if (data.activeCount) wakeSources.set(data.source, data.activeCount);
    else wakeSources.delete(data.source);
    publish();
  }));

  const owns = ctx => !stopped && owner === ctx.sessionManager;
  const rootContext = ctx =>
    ctx.mode === "tui" &&
    !/[/\\]senpi-task[/\\]children[/\\]/.test(ctx.sessionManager.getSessionFile() ?? "");
  pi.on("session_start", (_event, ctx) => {
    if (stopped || owner || !rootContext(ctx)) return;
    owner = ctx.sessionManager;
    context = ctx;
    active = !ctx.isIdle();
    publish();
    metadata();
  });
  pi.on("agent_start", (_event, ctx) => {
    if (!owns(ctx)) return;
    context = ctx;
    active = true;
    publish();
  });
  pi.on("agent_settled", (_event, ctx) => {
    if (!owns(ctx)) return;
    context = ctx;
    active = !ctx.isIdle();
    publish();
  });
  pi.on("session_before_compact", (_event, ctx) => {
    if (!owns(ctx)) return;
    compacting = true;
    publish();
  });
  const compactEnded = (event, ctx) => {
    if (!owns(ctx)) return;
    if (compacting && event.reason === "manual") active = false;
    compacting = false;
    publish();
  };
  pi.on("session_compact", compactEnded);
  pi.on("session_compact_failed", compactEnded);
  pi.on("model_select", (_event, ctx) => {
    if (!owns(ctx)) return;
    context = ctx;
    publish();
  });
  pi.on("thinking_level_select", (_event, ctx) => {
    if (!owns(ctx)) return;
    context = ctx;
    publish();
  });
  pi.on("ui_prompt_start", (event, ctx) => {
    if (stopped || !rootContext(ctx) || (owner && !owns(ctx))) return;
    prompt = event.title || "Waiting for input";
    publish();
  });
  pi.on("ui_prompt_end", (_event, ctx) => {
    if (stopped || !rootContext(ctx) || (owner && !owns(ctx))) return;
    prompt = undefined;
    publish();
  });
  pi.on("session_info_changed", (_event, ctx) => {
    if (!owns(ctx)) return;
    context = ctx;
    metadata();
  });
  pi.on("session_shutdown", async (event, ctx) => {
    if (owner && !owns(ctx)) return;
    stopped = true;
    for (const unsubscribe of subscriptions) unsubscribe();
    await reporter.close(Boolean(owner) && event.reason === "quit", event.signal);
  });
}

export default function extension(pi) {
  createIntegration(pi);
}
