// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// HERDR_INTEGRATION_ID=omp
// HERDR_INTEGRATION_VERSION=13
// @ts-nocheck

import net from "node:net";
import path from "node:path";

const HERDR_ENV = process.env.HERDR_ENV;
const socketPath = process.env.HERDR_SOCKET_PATH;
const socketEndpoint =
  process.platform === "win32" && socketPath ? `\\\\.\\pipe\\${socketPath}` : socketPath;
const paneId = process.env.HERDR_PANE_ID;
const source = "herdr:omp";

function enabled() {
  return HERDR_ENV === "1" && !!socketPath && !!paneId;
}

let requestQueue = Promise.resolve();

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

async function sendRequestNow(request: unknown): Promise<void> {
  if (await sendRequestAttempt(request, 500)) {
    return;
  }
  await sendRequestAttempt(request, 1500);
}

function sendRequest(request: unknown): Promise<void> {
  requestQueue = requestQueue.then(
    () => sendRequestNow(request),
    () => sendRequestNow(request),
  );
  return requestQueue;
}

type AgentState = "working" | "blocked" | "idle";

type QueuedState = {
  state: AgentState;
  message?: string;
  seq: number;
};

const idleDebounceMs = parseDurationEnv("HERDR_OMP_IDLE_DEBOUNCE_MS", 250);
const retryGraceMs = parseDurationEnv("HERDR_OMP_RETRY_GRACE_MS", 2500);
const retryableErrorPattern =
  /overloaded|provider.?returned.?error|rate.?limit|too many requests|429|500|502|503|504|service.?unavailable|server.?error|internal.?error|network.?error|connection.?error|connection.?refused|connection.?lost|websocket.?closed|websocket.?error|other side closed|fetch failed|upstream.?connect|reset before headers|socket hang up|ended without|http2 request did not get a response|timed? out|timeout|terminated|retry delay/i;
let reportSeq = Date.now() * 1000;
let currentAgentSessionId: string | undefined;
let currentAgentSessionPath: string | undefined;

function nextReportSeq(): number {
  reportSeq += 1;
  return reportSeq;
}

export function isAbsoluteSessionPath(file: unknown): file is string {
  return (
    typeof file === "string" &&
    (path.posix.isAbsolute(file) || path.win32.isAbsolute(file))
  );
}

function updateSessionRef(ctx: any): void {
  try {
    const file = ctx?.sessionManager?.getSessionFile?.();
    currentAgentSessionPath = isAbsoluteSessionPath(file) ? file : undefined;
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

function ensureSessionOnDisk(ctx: any): Promise<void> | undefined {
  const sessionManager = ctx?.sessionManager;
  if (typeof sessionManager?.ensureOnDisk !== "function") {
    return undefined;
  }
  try {
    return Promise.resolve(sessionManager.ensureOnDisk()).then(
      () => undefined,
      () => undefined,
    );
  } catch {
    return undefined;
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

function parseDurationEnv(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) {
    return fallback;
  }
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed < 0) {
    return fallback;
  }
  return parsed;
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

function isRecord(value: unknown): value is Record<string, any> {
  return !!value && typeof value === "object" && !Array.isArray(value);
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
  const phases = Array.isArray(input.phases) ? input.phases : undefined;
  const list = phases ?? input.list ?? input.steps ?? input.items ?? input.todos;
  if (!Array.isArray(list)) {
    return [];
  }
  return list.slice(0, 64).flatMap((step: any) => {
    if (typeof step === "string") {
      return [{ label: boundedLiveText(step), status: "pending" }];
    }
    if (!isRecord(step)) {
      return [];
    }
    const nested = [step.tasks, step.items, step.steps].find(Array.isArray);
    const candidates = nested ?? [step];
    return candidates.flatMap((candidate: any) => {
      if (typeof candidate === "string") {
        return [{ label: boundedLiveText(candidate), status: "pending" }];
      }
      if (!isRecord(candidate)) {
        return [];
      }
      const label = candidate.label ?? candidate.content ?? candidate.text ?? candidate.step;
      if (typeof label !== "string" || label.length === 0) {
        return [];
      }
      const rawStatus = candidate.status;
      const status =
        rawStatus === "completed" || rawStatus === "done"
          ? "completed"
          : rawStatus === "active" || rawStatus === "in_progress"
            ? "active"
            : rawStatus === "failed" || rawStatus === "blocked"
              ? "failed"
              : "pending";
      const prefix = typeof step.name === "string" ? `${step.name}: ` : "";
      return [{ label: boundedLiveText(`${prefix}${label}`), status }];
    });
  }).slice(0, 64);
}

function reportConversation(
  nativeId: string,
  turnId: string,
  payload: Record<string, unknown>,
): Promise<void> {
  const sessionRef = currentSessionRef();
  if (!sessionRef) {
    return Promise.resolve();
  }
  return sendRequest({
    id: `${source}:conversation:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "agent.conversation.report",
    params: {
      pane_id: paneId,
      source,
      agent: "omp",
      integration_token: process.env.HERDR_INTEGRATION_TOKEN ?? "",
      seq: nextReportSeq(),
      ...sessionRef,
      native_id: nativeId,
      turn_id: turnId,
      timestamp_ms: Date.now(),
      payload,
    },
  });
}

function reportSession(sessionStartSource = "startup"): Promise<void> {
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
      agent: "omp",
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
      agent: "omp",
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

function lastAssistantMessage(messages: unknown[]): any | undefined {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i] as any;
    if (message?.role === "assistant") {
      return message;
    }
  }
  return undefined;
}

function retryableErrorMessage(event: any): string | undefined {
  const messages = Array.isArray(event?.messages) ? event.messages : [];
  const assistant = lastAssistantMessage(messages);
  if (assistant?.stopReason !== "error") {
    return undefined;
  }

  const errorMessage = String(assistant.errorMessage ?? "");
  if (!retryableErrorPattern.test(errorMessage)) {
    return undefined;
  }
  return errorMessage || "retryable provider error";
}

function askBlockedMessage(args: any): string {
  const questions = Array.isArray(args?.questions) ? args.questions : [];
  const firstQuestion = questions.find((question: any) => typeof question?.question === "string");
  if (firstQuestion?.question) {
    return firstQuestion.question;
  }
  return "waiting for user input";
}

export default function (pi) {
  if (!enabled()) {
    return;
  }

  let agentActive = false;
  let retryHoldActive = false;
  let failureBlocked = false;
  let failureMessage: string | undefined;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  let lastState: AgentState | undefined;
  let lastMessage: string | undefined;
  let idleTimer: ReturnType<typeof setTimeout> | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let rootSession = false;
  let activeTurnId: string | undefined;
  let activeTurnStartedMs: number | undefined;
  let turnStartedReported = false;
  let activeMessageId: string | undefined;
  let messageOrdinal = 0;

  function clearTimer(timer: ReturnType<typeof setTimeout> | undefined) {
    if (timer) {
      clearTimeout(timer);
    }
  }

  function clearPendingTimers() {
    clearTimer(idleTimer);
    clearTimer(retryTimer);
    idleTimer = undefined;
    retryTimer = undefined;
  }

  function clearFailureState() {
    retryHoldActive = false;
    failureBlocked = false;
    failureMessage = undefined;
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
      void reportConversation(activeTurnId, activeTurnId, {
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

  function finishTurn(state: "completed" | "failed" = "completed") {
    turnStartedReported = false;
    activeMessageId = undefined;
    messageOrdinal = 0;
    if (!activeTurnId) {
      return;
    }
    const now = Date.now();
    void reportConversation(activeTurnId, activeTurnId, {
      type: "turn_state",
      state,
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

  function desiredState() {
    if (blockedCount > 0) {
      return { state: "blocked" as const, message: blockedMessage };
    }
    if (failureBlocked) {
      return { state: "blocked" as const, message: failureMessage };
    }
    if (agentActive || retryHoldActive) {
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

  function scheduleIdle() {
    clearPendingTimers();
    clearFailureState();
    idleTimer = setTimeout(() => {
      idleTimer = undefined;
      publishState();
    }, idleDebounceMs);
    idleTimer.unref?.();
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
    void reportConversation(
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
    const steps = toolName === "todo" || toolName === "plan" ? livePlanSteps(input) : [];
    void reportConversation(
      steps.length > 0 ? `plan:${activeTurnId}` : toolId,
      activeTurnId,
      steps.length > 0
        ? { type: "plan_update", steps }
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
      approval?.requestId ?? approval?.request_id ?? approval?.toolCallId ?? approval?.tool_call_id ?? approval?.id,
      `approval:${activeTurnId}`,
    );
    const decisions = Array.isArray(approval?.decisions)
      ? approval.decisions.slice(0, 16).flatMap((decision: any) =>
          typeof decision?.id === "string" && typeof decision?.label === "string"
            ? [{ id: decision.id.slice(0, 256), label: decision.label.slice(0, maxLiveText) }]
            : [],
        )
      : [
          { id: "allow", label: "Allow" },
          { id: "deny", label: "Deny" },
        ];
    void reportConversation(`approval:${requestId}`, activeTurnId, {
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

  function holdForRetry(message: string) {
    clearPendingTimers();
    retryHoldActive = true;
    failureBlocked = false;
    failureMessage = message;
    publishState();

    retryTimer = setTimeout(() => {
      retryTimer = undefined;
      retryHoldActive = false;
      failureBlocked = true;
      publishState();
    }, retryGraceMs);
    retryTimer.unref?.();
  }

  function activateRootSession(ctx: any, sessionStartSource = "startup"): boolean {
    if (ctx?.hasUI !== true) {
      return false;
    }
    rootSession = true;
    updateSessionRef(ctx);
    void reportSession(sessionStartSource);
    return true;
  }

  function resetSessionState() {
    clearPendingTimers();
    clearFailureState();
    agentActive = false;
    blockedCount = 0;
    blockedMessage = undefined;
    activeTurnId = undefined;
    activeTurnStartedMs = undefined;
    turnStartedReported = false;
    activeMessageId = undefined;
    messageOrdinal = 0;
  }

  function activateBlocked(message: string | undefined) {
    clearPendingTimers();
    blockedCount += 1;
    blockedMessage = message;
    publishState();
  }

  function deactivateBlocked() {
    blockedCount = Math.max(0, blockedCount - 1);
    if (blockedCount === 0) {
      blockedMessage = undefined;
    }
    publishState();
  }

  pi.events.on("herdr:blocked", (data) => {
    if (!rootSession) {
      return;
    }
    if (!data?.active) {
      deactivateBlocked();
      return;
    }

    activateBlocked(data.label);
  });

  pi.on("session_start", async (_event, ctx) => {
    const transcriptReady = ensureSessionOnDisk(ctx);
    if (transcriptReady) {
      await transcriptReady;
    }
    if (!activateRootSession(ctx)) {
      return;
    }
    // A reload can replace this extension mid-run without emitting another agent_start.
    agentActive = ctx?.isIdle?.() === false;
    publishState(true);
  });

  pi.on("session_switch", async (event, ctx) => {
    const transcriptReady = ensureSessionOnDisk(ctx);
    if (transcriptReady) {
      await transcriptReady;
    }
    if (!activateRootSession(ctx, event?.reason || "resume")) {
      return;
    }
    resetSessionState();
    publishState(true);
  });

  pi.on("agent_start", (_event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    updateSessionRef(ctx);
    void reportSession();
    clearPendingTimers();
    clearFailureState();
    activeTurnId = undefined;
    activeTurnStartedMs = Date.now();
    turnStartedReported = false;
    activeMessageId = undefined;
    messageOrdinal = 0;
    agentActive = true;
    publishState();
  });

  pi.on("tool_approval_requested", (event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    const label = event?.reason || `${event?.toolName || "Tool"} approval`;
    activateBlocked(label);
    reportApproval(event, "pending");
  });

  pi.on("tool_approval_resolved", (event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    deactivateBlocked();
    reportApproval(event, "resolved");
  });

  pi.on("tool_execution_start", (event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    reportTool(event, "running");
    if (event?.toolName !== "ask") {
      return;
    }
    activateBlocked(askBlockedMessage(event.args));
  });

  pi.on("tool_execution_end", (event, ctx) => {
    if (!rootSession && !activateRootSession(ctx)) {
      return;
    }
    const failed =
      event?.error ||
      event?.isError === true ||
      event?.result?.isError === true ||
      event?.result?.success === false ||
      event?.success === false;
    reportTool(event, failed ? "failed" : "completed");
    if (event?.toolName !== "ask") {
      return;
    }
    deactivateBlocked();
  });

  pi.on("agent_end", (event, ctx) => {
    if (!rootSession) {
      return;
    }
    updateSessionRef(ctx);
    void reportSession();
    if (!agentActive) {
      // OMP can emit duplicate/late end events while auto-retry is already
      // holding the pane in Working. Do not let an unqualified duplicate end
      // cancel the retry hold and publish a false Idle.
      return;
    }

    agentActive = false;

    const retryableMessage = retryableErrorMessage(event);
    if (retryableMessage) {
      holdForRetry(retryableMessage);
      return;
    }

    scheduleIdle();
    finishTurn();
  });

  pi.on("session_shutdown", () => {
    if (rootSession) {
      clearPendingTimers();
      finishTurn("failed");
    }
  });
}
