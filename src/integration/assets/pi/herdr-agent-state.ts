// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// HERDR_INTEGRATION_ID=pi
// HERDR_INTEGRATION_VERSION=11
// @ts-nocheck

import net from "node:net";
import path from "node:path";

const HERDR_ENV = process.env.HERDR_ENV;
const socketPath = process.env.HERDR_SOCKET_PATH;
const socketEndpoint =
  process.platform === "win32" && socketPath ? `\\\\.\\pipe\\${socketPath}` : socketPath;
const paneId = process.env.HERDR_PANE_ID;
const source = "herdr:pi";

function enabled() {
  return HERDR_ENV === "1" && !!socketPath && !!paneId;
}

function sendRequestAttempt(request: unknown, timeoutMs: number): Promise<boolean> {
  if (!enabled()) {
    return Promise.resolve(true);
  }

  return new Promise((resolve) => {
    let done = false;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const finish = (delivered: boolean) => {
      if (done) return;
      done = true;
      if (timeout) {
        clearTimeout(timeout);
      }
      socket.destroy();
      resolve(delivered);
    };

    const socket = net.createConnection(socketEndpoint!);
    socket.on("error", () => finish(false));
    socket.on("connect", () => socket.write(`${JSON.stringify(request)}\n`));
    socket.on("data", () => finish(true));
    socket.on("end", () => finish(false));
    timeout = setTimeout(() => finish(false), timeoutMs);
    timeout.unref?.();
  });
}

async function sendRequest(request: unknown): Promise<void> {
  if (await sendRequestAttempt(request, 500)) {
    return;
  }
  await sendRequestAttempt(request, 1500);
}

type AgentState = "working" | "blocked" | "idle";

type QueuedState = {
  state: AgentState;
  message?: string;
  seq: number;
};

let reportSeq = Date.now() * 1000;
let currentAgentSessionId: string | undefined;
let currentAgentSessionPath: string | undefined;
let conversationReportQueue = Promise.resolve();

function nextReportSeq(): number {
  reportSeq += 1;
  return reportSeq;
}

function updateSessionRef(ctx: any): void {
  try {
    const file = ctx?.sessionManager?.getSessionFile?.();
    currentAgentSessionPath =
      typeof file === "string" && file.startsWith("/") ? file : undefined;
  } catch {
    currentAgentSessionPath = undefined;
  }

  try {
    const id = ctx?.sessionManager?.getSessionId?.();
    currentAgentSessionId = typeof id === "string" && id.length > 0 ? id : undefined;
  } catch {
    currentAgentSessionId = undefined;
  }
}

function withSessionRef(params: Record<string, unknown>): Record<string, unknown> {
  if (currentAgentSessionPath) {
    return { ...params, agent_session_path: currentAgentSessionPath };
  }
  if (currentAgentSessionId) {
    return { ...params, agent_session_id: currentAgentSessionId };
  }
  return params;
}

function currentSessionRef(): Record<string, unknown> | undefined {
  if (currentAgentSessionPath) {
    return { agent_session_path: currentAgentSessionPath };
  }
  if (currentAgentSessionId) {
    return { agent_session_id: currentAgentSessionId };
  }
  return undefined;
}

const maxLiveText = 16_384;
const maxLivePaths = 64;

function boundedLiveText(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value.slice(0, maxLiveText) : fallback;
}

function liveId(value: unknown, fallback: string): string {
  return typeof value === "string" && value.length > 0 && value.length <= 256 ? value : fallback;
}

function messageText(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }
  if (Array.isArray(value)) {
    return value
      .map((part) => {
        if (!isRecord(part)) return "";
        const type = typeof part.type === "string" ? part.type : undefined;
        if (type && !["text", "input_text", "output_text"].includes(type)) return "";
        return messageText(part.text ?? part.content);
      })
      .join("");
  }
  if (isRecord(value)) {
    if (["thinking", "toolCall", "toolResult"].includes(String(value.type ?? ""))) {
      return "";
    }
    return messageText(value.content ?? value.text ?? value.message);
  }
  return "";
}

function messageTurnId(message: any): string | undefined {
  const timestamp = message?.timestamp;
  return typeof timestamp === "number" && Number.isSafeInteger(timestamp) && timestamp >= 0
    ? `turn:${timestamp}`
    : undefined;
}

function isRecord(value: unknown): value is Record<string, any> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function livePaths(value: unknown): string[] {
  const candidates: unknown[] = [];
  if (isRecord(value)) {
    for (const key of ["path", "file_path", "notebook_path"]) {
      candidates.push(value[key]);
    }
    if (Array.isArray(value.paths)) {
      candidates.push(...value.paths);
    }
  }
  return [...new Set(candidates.map(safeLivePath).filter(Boolean))].slice(0, maxLivePaths);
}

function safeLivePath(value: unknown): string | undefined {
  let candidate = boundedLiveText(value, "");
  if (!candidate || candidate.includes("\0")) {
    return undefined;
  }
  if (path.isAbsolute(candidate)) {
    const relative = path.relative(process.cwd(), candidate);
    if (
      !relative ||
      relative === ".." ||
      relative.startsWith(`..${path.sep}`) ||
      path.isAbsolute(relative)
    ) {
      return undefined;
    }
    candidate = relative;
  } else if (path.posix.isAbsolute(candidate) || path.win32.isAbsolute(candidate)) {
    return undefined;
  }
  if (
    candidate.startsWith("/") ||
    candidate.startsWith("\\") ||
    candidate.split(/[\\/]/).some((part) => part === ".." || part.length === 0)
  ) {
    return undefined;
  }
  return candidate;
}

function liveCommandPreview(toolName: string, value: unknown): string | undefined {
  const action = toolName.toLowerCase();
  if (
    !action.includes("bash") &&
    !action.includes("shell") &&
    !["exec_command", "execute_command", "run_command"].includes(action)
  ) {
    return undefined;
  }
  let input = value;
  if (typeof input === "string") {
    try {
      input = JSON.parse(input);
    } catch {
      return boundedLiveText(input) || undefined;
    }
  }
  if (!isRecord(input)) {
    return undefined;
  }
  const command = input.command ?? input.cmd;
  return typeof command === "string" && command.length > 0
    ? boundedLiveText(command)
    : undefined;
}

function livePlanSteps(value: unknown): Array<Record<string, unknown>> {
  const input = isRecord(value) ? value : {};
  const list = input.list ?? input.steps ?? input.items ?? input.todos;
  if (!Array.isArray(list)) {
    return [];
  }
  return list.slice(0, 64).flatMap((step) => {
    if (typeof step === "string") {
      return [{ label: boundedLiveText(step), status: "pending" }];
    }
    if (!isRecord(step)) {
      return [];
    }
    const label = step.label ?? step.content ?? step.text ?? step.step;
    if (typeof label !== "string" || label.length === 0) {
      return [];
    }
    const rawStatus = step.status;
    const status =
      rawStatus === "completed" || rawStatus === "done"
        ? "completed"
        : rawStatus === "active" || rawStatus === "in_progress"
          ? "active"
          : rawStatus === "failed" || rawStatus === "blocked"
            ? "failed"
            : "pending";
    return [{ label: boundedLiveText(label), status }];
  });
}

function queueConversationReport(
  nativeId: string,
  turnId: string,
  payload: Record<string, unknown>,
): void {
  const sessionRef = currentSessionRef();
  if (!sessionRef) {
    return;
  }
  const request = {
    id: `${source}:conversation:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "agent.conversation.report",
    params: {
      pane_id: paneId,
      source,
      agent: "pi",
      integration_token: process.env.HERDR_INTEGRATION_TOKEN ?? "",
      seq: nextReportSeq(),
      ...sessionRef,
      native_id: nativeId,
      turn_id: turnId,
      timestamp_ms: Date.now(),
      payload,
    },
  };
  conversationReportQueue = conversationReportQueue.then(
    () => sendRequest(request),
    () => sendRequest(request),
  );
}

function reportSession(sessionStartSource?: string): Promise<void> {
  const sessionRef = currentSessionRef();
  if (!sessionRef) {
    return Promise.resolve();
  }

  return sendRequest({
    id: `${source}:session:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.report_agent_session",
    params: {
      pane_id: paneId,
      source,
      agent: "pi",
      seq: nextReportSeq(),
      session_start_source: sessionStartSource,
      integration_token: process.env.HERDR_INTEGRATION_TOKEN ?? "",
      ...sessionRef,
    },
  });
}

function sendState(state: AgentState, message?: string, seq = nextReportSeq()): Promise<void> {
  return sendRequest({
    id: `${source}:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.report_agent",
    params: withSessionRef({
      pane_id: paneId,
      source,
      agent: "pi",
      state,
      message,
      seq,
    }),
  });
}

let sendInFlight = false;
let queuedState: QueuedState | undefined;

function queueState(state: AgentState, message?: string): void {
  queuedState = { state, message, seq: nextReportSeq() };
  if (!sendInFlight) {
    void drainStateQueue();
  }
}

async function drainStateQueue(): Promise<void> {
  if (sendInFlight) {
    return;
  }

  sendInFlight = true;
  try {
    while (queuedState) {
      const next = queuedState;
      queuedState = undefined;
      await sendState(next.state, next.message, next.seq);
    }
  } finally {
    sendInFlight = false;
    if (queuedState) {
      void drainStateQueue();
    }
  }
}

export default function (pi) {
  if (!enabled()) {
    return;
  }

  let agentActive = false;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  let lastState: AgentState | undefined;
  let lastMessage: string | undefined;
  let rootSession = false;
  let activeTurnId: string | undefined;
  let activeTurnStartedMs: number | undefined;
  let turnStartedReported = false;
  let activeMessageId: string | undefined;
  let messageOrdinal = 0;

  function desiredState() {
    if (blockedCount > 0) {
      return { state: "blocked" as const, message: blockedMessage };
    }
    if (agentActive) {
      return { state: "working" as const, message: undefined };
    }
    return { state: "idle" as const, message: undefined };
  }

  function publishState(force = false) {
    const next = desiredState();
    if (!force && next.state === lastState && next.message === lastMessage) {
      return;
    }
    lastState = next.state;
    lastMessage = next.message;
    queueState(next.state, next.message);
  }

  function ensureTurn(turnId?: string, startedMs?: number): void {
    if (turnId) {
      activeTurnId = turnId;
    } else if (!activeTurnId) {
      activeTurnId = `${source}:turn:${Date.now()}:${Math.random().toString(36).slice(2)}`;
    }
    if (startedMs !== undefined) {
      activeTurnStartedMs = startedMs;
    } else if (activeTurnStartedMs === undefined) {
      activeTurnStartedMs = Date.now();
    }
    if (!turnStartedReported && activeTurnId) {
      turnStartedReported = true;
      queueConversationReport(activeTurnId, activeTurnId, {
        type: "turn_state",
        state: "started",
        started_ms: activeTurnStartedMs,
      });
    }
  }

  function nextMessageId(role: string, message?: any): string {
    ensureTurn();
    if (role === "user") {
      return `user:${activeTurnId}`;
    }
    const timestamp = message?.timestamp;
    if (typeof timestamp === "number" && Number.isSafeInteger(timestamp) && timestamp >= 0) {
      return `message:${activeTurnId}:${timestamp}`;
    }
    const id = `message:${activeTurnId}:${messageOrdinal}`;
    messageOrdinal += 1;
    return id;
  }

  function reportMessage(event: any, phase: "commentary" | "final") {
    if (!rootSession) {
      return;
    }
    const message = event?.message ?? event;
    if (message?.role !== "user" && message?.role !== "assistant") {
      return;
    }
    ensureTurn(
      message?.role === "user" ? messageTurnId(message) : undefined,
      message?.role === "user" ? message?.timestamp : undefined,
    );
    const text = boundedLiveText(messageText(message));
    if (!text || !activeTurnId) {
      return;
    }
    const role = message?.role === "user" ? "user_message" : "assistant_message";
    activeMessageId ??= nextMessageId(
      message?.role === "user" ? "user" : "assistant",
      message,
    );
    queueConversationReport(
      activeMessageId,
      activeTurnId,
      role === "user_message"
        ? { type: role, text, attachments: [] }
        : { type: role, phase, text, state: "completed" },
    );
  }

  function reportTool(event: any, status: "running" | "completed" | "failed") {
    if (!rootSession) {
      return;
    }
    const tool = event?.toolCall ?? event;
    const toolName = boundedLiveText(tool?.toolName ?? tool?.name ?? "tool", "tool");
    ensureTurn();
    const toolId = liveId(
      tool?.toolCallId ?? tool?.callId ?? tool?.id,
      `tool:${activeTurnId}:${toolName}`,
    );
    const input = tool?.args ?? tool?.arguments ?? tool?.input;
    const planSteps = toolName === "todo" || toolName === "plan" ? livePlanSteps(input) : [];
    queueConversationReport(
      planSteps.length > 0 ? `plan:${activeTurnId}` : toolId,
      activeTurnId,
      planSteps.length > 0
        ? { type: "plan_update", steps: planSteps }
        : {
            type: "tool_activity",
            action: toolName,
            label: toolName,
            status,
            preview: liveCommandPreview(toolName, input),
            detail: boundedLiveText(tool?.detail ?? tool?.result?.output),
            paths: livePaths(input),
          },
    );
  }

  function reportApproval(event: any, status: "pending" | "resolved") {
    if (!rootSession) {
      return;
    }
    ensureTurn();
    const approval = event?.approval ?? event;
    const requestId = liveId(
      approval?.requestId ??
        approval?.request_id ??
        approval?.toolCallId ??
        approval?.tool_call_id ??
        approval?.id,
      `approval:${activeTurnId}`,
    );
    const decisions = Array.isArray(approval?.decisions)
      ? approval.decisions
          .slice(0, 16)
          .flatMap((decision: any) =>
            typeof decision?.id === "string" && typeof decision?.label === "string"
              ? [{ id: decision.id.slice(0, 256), label: decision.label.slice(0, maxLiveText) }]
              : [],
          )
      : [
          { id: "allow", label: "Allow" },
          { id: "deny", label: "Deny" },
        ];
    queueConversationReport(`approval:${requestId}`, activeTurnId, {
      type: "approval",
      request_id: requestId,
      prompt: boundedLiveText(approval?.reason ?? approval?.prompt ?? "Approval required"),
      decisions,
      status,
      selected_decision:
        status === "resolved"
          ? boundedLiveText(approval?.decisionId ?? approval?.decision_id)
          : undefined,
      structured_response: false,
    });
  }

  pi.on("message_start", (event) => {
    const message = event?.message ?? event;
    if (message?.role === "user") {
      const startedMs =
        typeof message?.timestamp === "number" && Number.isSafeInteger(message.timestamp)
          ? message.timestamp
          : Date.now();
      activeTurnId = messageTurnId(message);
      activeTurnStartedMs = startedMs;
      turnStartedReported = false;
      messageOrdinal = 0;
    }
    if (message?.role !== "user" && message?.role !== "assistant") {
      return;
    }
    ensureTurn(
      message?.role === "user" ? messageTurnId(message) : undefined,
      message?.role === "user" ? message?.timestamp : undefined,
    );
    activeMessageId =
      message?.role === "assistant" && !messageText(message)
        ? undefined
        : nextMessageId(message?.role === "user" ? "user" : "assistant", message);
    if (message?.role === "user") {
      reportMessage(event, "final");
    }
  });
  pi.on("message_update", (event) => reportMessage(event, "commentary"));
  pi.on("message_end", (event) => reportMessage(event, "final"));
  pi.on("tool_execution_start", (event) => reportTool(event, "running"));
  pi.on("tool_execution_end", (event) => {
    const failed =
      event?.error ||
      event?.isError === true ||
      event?.result?.isError === true ||
      event?.result?.success === false ||
      event?.success === false;
    reportTool(event, failed ? "failed" : "completed");
  });
  pi.on("tool_approval_requested", (event) => reportApproval(event, "pending"));
  pi.on("tool_approval_resolved", (event) => reportApproval(event, "resolved"));

  pi.events.on("herdr:blocked", (data) => {
    if (!rootSession) {
      return;
    }
    if (!data?.active) {
      blockedCount = Math.max(0, blockedCount - 1);
      if (blockedCount === 0) {
        blockedMessage = undefined;
      }
      publishState();
      return;
    }

    blockedCount += 1;
    blockedMessage = data.label;
    publishState();
  });

  pi.on("session_start", async (event, ctx) => {
    // TUI only: RPC/JSON/print modes are headless (no PTY herdr can display),
    // and RPC still reports hasUI=true, so mode is the reliable gate.
    if (ctx?.mode !== "tui") {
      return;
    }
    rootSession = true;
    updateSessionRef(ctx);
    await reportSession(event?.reason);
    // A reload can replace this extension mid-run without emitting another agent_start.
    agentActive = ctx?.isIdle?.() === false;
    publishState(true);
  });

  pi.on("agent_start", (_event, ctx) => {
    if (!rootSession) {
      return;
    }
    updateSessionRef(ctx);
    void reportSession();
    activeTurnId = undefined;
    activeTurnStartedMs = Date.now();
    turnStartedReported = false;
    activeMessageId = undefined;
    messageOrdinal = 0;
    agentActive = true;
    publishState();
  });

  pi.on("agent_settled", (_event, ctx) => {
    if (!rootSession || ctx?.isIdle?.() !== true) {
      return;
    }

    agentActive = false;
    turnStartedReported = false;
    activeMessageId = undefined;
    messageOrdinal = 0;
    if (activeTurnId) {
      const now = Date.now();
      queueConversationReport(activeTurnId, activeTurnId, {
        type: "turn_state",
        state: "completed",
        started_ms: activeTurnStartedMs,
        duration_ms:
          activeTurnStartedMs === undefined ? undefined : Math.max(0, now - activeTurnStartedMs),
      });
      activeTurnId = undefined;
      activeTurnStartedMs = undefined;
      turnStartedReported = false;
      activeMessageId = undefined;
      messageOrdinal = 0;
    }
    publishState();
  });
}
