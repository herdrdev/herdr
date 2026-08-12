import { afterEach, beforeEach, expect, mock, test } from "bun:test";

const requests: unknown[] = [];
const activeDisposers: Array<() => void> = [];
const requestWaiters: Array<() => void> = [];
let importCounter = 0;

mock.module("node:net", () => ({
  default: {
    createConnection(_path: string, onConnect: () => void) {
      const handlers = new Map<string, () => void>();
      const client = {
        write(input: string) {
          requests.push(JSON.parse(input.trim()));
          requestWaiters.shift()?.();
          queueMicrotask(() => client.emit("data"));
        },
        setTimeout() {},
        on(event: string, handler: () => void) {
          handlers.set(event, handler);
        },
        destroy() {},
        emit(event: string) {
          handlers.get(event)?.();
        },
      };
      queueMicrotask(onConnect);
      return client;
    },
  },
}));

beforeEach(() => {
  requests.length = 0;
  requestWaiters.length = 0;
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
