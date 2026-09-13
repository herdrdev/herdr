import { afterEach, beforeEach, expect, mock, test } from "bun:test";

const requests: unknown[] = [];
const activeDisposers: Array<() => void> = [];
const requestWaiters: Array<() => void> = [];
const stateWaiters: Array<() => void> = [];
let importCounter = 0;
let holdConnections = false;
let failConnections = false;
const connections: Array<() => void> = [];

mock.module("node:net", () => ({
  default: {
    createConnection(_path: string, onConnect: () => void) {
      const handlers = new Map<string, () => void>();
      const client = {
        destroyed: false,
        write(input: string) {
          if (client.destroyed) return;
          const request = JSON.parse(input.trim());
          requests.push(request);
          if (isRecord(request) && isRecord(request.params) && request.params.state !== undefined) {
            stateWaiters.shift()?.();
          }
          requestWaiters.shift()?.();
          queueMicrotask(() => client.emit("data"));
        },
        setTimeout() {},
        on(event: string, handler: () => void) {
          handlers.set(event, handler);
        },
        destroy() {
          client.destroyed = true;
        },
        emit(event: string) {
          handlers.get(event)?.();
        },
      };
      if (holdConnections) connections.push(onConnect);
      else if (failConnections) queueMicrotask(() => client.emit("error"));
      else queueMicrotask(onConnect);
      return client;
    },
  },
}));

beforeEach(() => {
  requests.length = 0;
  requestWaiters.length = 0;
  stateWaiters.length = 0;
  holdConnections = false;
  failConnections = false;
  connections.length = 0;
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = "test.sock";
  process.env.HERDR_PANE_ID = "test:p1";
});

afterEach(() => {
  for (const dispose of activeDisposers.splice(0)) {
    dispose();
  }
});

async function loadPlugin() {
  importCounter += 1;
  const module = await import(`./herdr-tui-session.js?test=${importCounter}`);
  return module.default;
}

function fakeApi() {
  const sessions = new Map<string, { id: string; parentID?: string }>();
  let current: { name: string; params?: { sessionID: string } } = { name: "home" };
  let dispose: (() => void) | undefined;
  activeDisposers.push(() => dispose?.());

  return {
    api: {
      route: {
        get current() {
          return current;
        },
      },
      state: {
        session: {
          get(sessionID: string) {
            return sessions.get(sessionID);
          },
        },
      },
      lifecycle: {
        onDispose(handler: () => void) {
          dispose = handler;
          return () => {};
        },
      },
    },
    addSession(session: { id: string; parentID?: string }) {
      sessions.set(session.id, session);
    },
    select(sessionID: string) {
      current = { name: "session", params: { sessionID } };
    },
    dispose() {
      dispose?.();
    },
  };
}

function waitForNextRequest(): Promise<void> {
  return new Promise((resolve) => requestWaiters.push(resolve));
}

function waitForStateReport(): Promise<void> {
  return new Promise((resolve) => stateWaiters.push(resolve));
}

test("reports a root session when only the local route changes", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  await plugin.tui(tui.api);

  const dispatched = waitForNextRequest();
  tui.select("session-a");
  await dispatched;

  expect(requests).toHaveLength(1);
  expect(requestParam(requests[0], "agent_session_id")).toBe("session-a");
  expect(requestParam(requests[0], "session_start_source")).toBe("select");
  expect(requestParam(requests[0], "seq")).toBeUndefined();
});

test("retries an initial selection while Herdr detects the process", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.select("session-a");

  await plugin.tui(tui.api);
  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests.map((request) => requestParam(request, "agent_session_id"))).toEqual([
    "session-a",
    "session-a",
  ]);
});

test("does not report root sessions not selected by this TUI", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  tui.addSession({ id: "session-b" });
  tui.select("session-a");
  await plugin.tui(tui.api);

  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests.length).toBeGreaterThan(0);
  expect(requests.every((request) => requestParam(request, "agent_session_id") === "session-a")).toBe(
    true,
  );
});

test("does not replace the root session with a selected child session", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "root-session" });
  tui.addSession({ id: "child-session", parentID: "root-session" });
  tui.select("root-session");
  await plugin.tui(tui.api);
  expect(requests).toHaveLength(1);

  tui.select("child-session");
  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests).toHaveLength(1);
  expect(requestParam(requests[0], "agent_session_id")).toBe("root-session");
});

test("stops route polling when the TUI plugin is disposed", async () => {
  const plugin = await loadPlugin();
  const tui = fakeApi();
  tui.addSession({ id: "session-a" });
  await plugin.tui(tui.api);
  tui.dispose();
  tui.select("session-a");

  await new Promise((resolve) => setTimeout(resolve, 125));

  expect(requests).toHaveLength(0);
});

function requestParam(request: unknown, name: string): unknown {
  if (!isRecord(request) || !isRecord(request.params)) {
    return undefined;
  }
  return request.params[name];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function v2Api() {
  const sessions = new Map([
    ["a", { id: "a" }],
    ["b", { id: "b" }],
    ["child", { id: "child", parentID: "a" }],
  ]);
  let route = { type: "session", sessionID: "a" };
  const listeners = new Set<(event: unknown) => void>();
  const permissions = new Map<string, Array<{ id: string }> | undefined>();
  const forms = new Map<string, Array<{ id: string }> | undefined>();
  return {
    api: {
      ui: { router: { current: () => route } },
      data: {
        session: {
          get: (id: string) => sessions.get(id),
          family: () => [...sessions.keys()],
          status: () => "idle",
          permission: { list: (id: string) => permissions.get(id) },
          form: { list: (id: string) => forms.get(id) },
        },
        listen: (handler: (event: unknown) => void) => {
          listeners.add(handler);
          return () => listeners.delete(handler);
        },
      },
    },
    select(sessionID: string) { route = { type: "session", sessionID }; },
    home() { route = { type: "home", sessionID: "" }; },
    emit(type: string, data?: object) {
      for (const listener of listeners) listener({ details: { type, data } });
    },
    listeners,
    sessions,
    permissions,
    forms,
  };
}

const flushReports = () => new Promise((resolve) => setTimeout(resolve, 10));
const states = () => requests.filter((r) => requestParam(r, "state") !== undefined)
  .map((r) => requestParam(r, "state"));

test("V2 ignores events without data", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await flushReports();
  requests.length = 0;
  expect(() => tui.emit("legacy.event")).not.toThrow();
  tui.emit("session.execution.started", { sessionID: "a" });
  await flushReports();
  expect(states()).toEqual(["working"]);
});

test("V2 completes and interrupts without legacy idle events", async () => {
  for (const terminal of ["succeeded", "interrupted", "failed"]) {
    const plugin = await loadPlugin();
    const tui = v2Api();
    const dispose = await plugin.setup(tui.api);
    activeDisposers.push(dispose);
    await flushReports();
    requests.length = 0;
    tui.emit("session.execution.started", { sessionID: "a" });
    tui.emit(`session.execution.${terminal}`, { sessionID: "a" });
    await flushReports();
    expect(states()).toEqual(["working", terminal === "failed" ? "blocked" : "idle"]);
    dispose();
  }
});

test("V2 aggregates root and child blockers and ignores other roots and child completion", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await flushReports();
  requests.length = 0;
  tui.emit("session.execution.started", { sessionID: "a" });
  tui.emit("permission.asked", { sessionID: "a", id: "permission-a" });
  tui.emit("form.created", { form: { sessionID: "child", id: "form-child" } });
  tui.emit("permission.replied", { sessionID: "a", requestID: "permission-a" });
  tui.emit("session.execution.succeeded", { sessionID: "child" });
  tui.emit("session.execution.started", { sessionID: "b" });
  tui.emit("permission.asked", { sessionID: "b", id: "other" });
  await flushReports();
  expect(states().at(-1)).toBe("blocked");
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "a")).toBe(true);
  tui.emit("form.cancelled", { sessionID: "child", id: "form-child" });
  tui.emit("session.execution.succeeded", { sessionID: "a" });
  await flushReports();
  expect(states().slice(-2)).toEqual(["working", "idle"]);
});

test("V2 discards queued reports after selection changes and stops on disposal", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await flushReports();
  requests.length = 0;
  tui.emit("session.execution.started", { sessionID: "a" });
  tui.select("b");
  tui.emit("session.execution.started", { sessionID: "b" });
  await flushReports();
  expect(requests.every((r) => requestParam(r, "agent_session_id") === "b")).toBe(true);
  requests.length = 0;
  tui.emit("session.execution.succeeded", { sessionID: "b" });
  tui.home();
  await flushReports();
  expect(requests).toHaveLength(0);
  dispose();
  expect(tui.listeners.size).toBe(0);
  tui.select("a");
  await new Promise((resolve) => setTimeout(resolve, 250));
  expect(requests).toHaveLength(0);
});

test("V2 reconciles late blocker hydration without reviving an already-replied request", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await flushReports();
  tui.permissions.set("child", [{ id: "late" }]);
  await waitForStateReport();
  expect(states().at(-1)).toBe("blocked");
  tui.emit("permission.replied", { sessionID: "child", requestID: "late" });
  await waitForStateReport();
  expect(states().at(-1)).toBe("idle");
  tui.permissions.set("child", []);
  tui.forms.set("child", [{ id: "second" }]);
  await waitForStateReport();
  expect(states().at(-1)).toBe("blocked");
  tui.sessions.delete("child");
  tui.emit("session.deleted", { sessionID: "child" });
  await flushReports();
  expect(states().at(-1)).toBe("idle");
});

test("V2 never writes a delayed connection after disposal or a session switch", async () => {
  for (const action of ["dispose", "switch"]) {
    const plugin = await loadPlugin();
    const tui = v2Api();
    holdConnections = true;
    requests.length = 0;
    const dispose = await plugin.setup(tui.api);
    activeDisposers.push(dispose);
    await flushReports();
    expect(connections.length).toBeGreaterThan(0);
    if (action === "dispose") dispose();
    else tui.select("b");
    holdConnections = false;
    for (const connect of connections.splice(0)) connect();
    await flushReports();
    expect(requests).toHaveLength(0);
    dispose();
  }
});

test("V2 settles a connection that never completes", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  holdConnections = true;
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  const started = Date.now();
  while (connections.length <= 1 && Date.now() - started < 2_000) {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  expect(connections.length).toBeGreaterThan(1);
  dispose();
});

test("V2 resends the latest state after a failed delivery", async () => {
  const plugin = await loadPlugin();
  const tui = v2Api();
  const dispose = await plugin.setup(tui.api);
  activeDisposers.push(dispose);
  await flushReports();
  // Exhaust the selection retry schedule so only the event report remains.
  await new Promise((resolve) => setTimeout(resolve, 1_600));
  requests.length = 0;
  tui.emit("session.execution.started", { sessionID: "a" });
  await flushReports();
  failConnections = true;
  tui.emit("session.execution.succeeded", { sessionID: "a" });
  const resend = waitForStateReport();
  await new Promise((resolve) => setTimeout(resolve, 700));
  failConnections = false;
  await resend;
  expect(states().at(-1)).toBe("idle");
  dispose();
});
