import { beforeEach, expect, mock, test } from "bun:test";

const requests: unknown[] = [];
const clients: FakeClient[] = [];
const requestWaiters: Array<() => void> = [];
let autoAcknowledge = true;
let importCounter = 0;
const originalArgv = [...process.argv];

type FakeClient = {
  emit: (event: string) => void;
};

mock.module("node:net", () => ({
  default: {
    createConnection(_path: string, onConnect: () => void) {
      const handlers = new Map<string, () => void>();
      const client = {
        write(input: string) {
          requests.push(JSON.parse(input.trim()));
          requestWaiters.shift()?.();
          if (autoAcknowledge) {
            queueMicrotask(() => client.emit("data"));
          }
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
      clients.push(client);
      queueMicrotask(onConnect);
      return client;
    },
  },
}));

beforeEach(() => {
  requests.length = 0;
  clients.length = 0;
  requestWaiters.length = 0;
  autoAcknowledge = true;
  process.env.HERDR_ENV = "1";
  process.env.HERDR_SOCKET_PATH = "test.sock";
  process.env.HERDR_PANE_ID = "test:p1";
  process.argv = originalArgv;
});

async function loadPlugin() {
  importCounter += 1;
  const { HerdrAgentStatePlugin } = await import(`./herdr-agent-state.js?test=${importCounter}`);
  return HerdrAgentStatePlugin();
}

function waitForNextRequest(): Promise<void> {
  return new Promise((resolve) => requestWaiters.push(resolve));
}

test("serializes lifecycle reports", async () => {
  autoAcknowledge = false;
  const plugin = await loadPlugin();
  const firstDispatched = waitForNextRequest();
  const working = plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "busy" } },
    },
  });
  await firstDispatched;

  const secondDispatched = waitForNextRequest();
  const idle = plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "idle" } },
    },
  });
  expect(clients).toHaveLength(1);

  clients[0]?.emit("data");
  await secondDispatched;
  expect(clients).toHaveLength(2);
  clients[1]?.emit("data");
  await Promise.all([working, idle]);

  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  const sequences = requests.map(requestSeq);
  expect(sequences[0]).toEqual(expect.any(Number));
  expect(sequences[1]).toBe((sequences[0] as number) + 1);
});

test("suppresses redundant same-session updates", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: { type: "session.updated", properties: { sessionID: "root-session" } },
  });
  await plugin.event({
    event: { type: "session.updated", properties: { sessionID: "replacement-session" } },
  });

  expect(requests.map(requestMethod)).toEqual([
    "pane.report_agent",
    "pane.report_agent_session",
  ]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", "replacement-session"]);
});

test("reports retry status as working", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "retry" } },
    },
  });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestState)).toEqual(["working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session"]);
});

test("reports child prompts without replacing the root session", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });

  for (const type of ["permission.asked", "question.asked"]) {
    await plugin.event({ event: { type, properties: { sessionID: "child-session" } } });
  }
  for (const type of ["permission.replied", "question.replied", "question.rejected"]) {
    await plugin.event({ event: { type, properties: { sessionID: "child-session" } } });
  }

  expect(requests.map(requestState)).toEqual([
    "blocked",
    "blocked",
    "working",
    "working",
    "working",
  ]);
  expect(requests.map(requestSessionID)).toEqual([
    undefined,
    undefined,
    undefined,
    undefined,
    undefined,
  ]);
});

test("aggregates child state and restores the root state after children idle", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "idle" } },
    },
  });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child-session" } } });

  expect(requests.map(requestState)).toEqual(["working", "working", "working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual([
    "root-session",
    undefined,
    "root-session",
    undefined,
  ]);
});

test("returns idle after a child finishes before any root state arrives", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child-session" } } });

  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual([undefined, undefined]);
});

test("clears child state when a new root replaces the owned tree", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-a", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-c",
        info: { id: "child-c", parentID: "root-a" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-c", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.created", properties: { sessionID: "root-b" } } });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "root-b" } } });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-c", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestState)).toEqual(["working", "working", undefined, "idle"]);
  expect(requests.map(requestSessionID)).toEqual(["root-a", undefined, "root-b", "root-b"]);
});

test("ignores a deleted prior root after replacement", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-a", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-a",
        info: { id: "child-a", parentID: "root-a" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-a", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.created", properties: { sessionID: "root-b" } } });
  await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "root-a" } } });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-b",
        info: { id: "child-b", parentID: "root-b" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-b", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestState)).toEqual(["working", "working", undefined, "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-a", undefined, "root-b", undefined]);
});

test("does not let a prior root update reclaim ownership", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-a", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.created", properties: { sessionID: "root-b" } } });
  await plugin.event({ event: { type: "session.updated", properties: { sessionID: "root-a" } } });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-b", status: { type: "idle" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-b", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestState)).toEqual(["working", undefined, undefined, "idle", "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-a", "root-b", "root-a", "root-b", "root-b"]);
});

test("clears deleted child descendants from the aggregate", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "idle" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-c",
        info: { id: "child-c", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-d",
        info: { id: "child-d", parentID: "child-c" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-d", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "permission.asked", properties: { sessionID: "child-d" } } });
  await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "child-c" } } });
  await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "child-d" } } });

  expect(requests.map(requestState)).toEqual(["idle", "working", "blocked", "idle"]);
  expect(requests.map(requestSessionID)).toEqual(["root-session", undefined, undefined, undefined]);
});

test("resets a deleted root and allows a new root to take over", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "blocked" } },
    },
  });
  await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "root-session" } } });
  await plugin.event({ event: { type: "session.created", properties: { sessionID: "new-root" } } });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "new-root", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestState)).toEqual([
    "working",
    "working",
    "blocked",
    "idle",
    undefined,
    "working",
  ]);
  expect(requests.map(requestSessionID)).toEqual([
    "root-session",
    undefined,
    "root-session",
    undefined,
    "new-root",
    "new-root",
  ]);
});

test("ignores a late child creation while waiting for a new root", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-a", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-c",
        info: { id: "child-c", parentID: "root-a" },
      },
    },
  });
  await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "root-a" } } });

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-c",
        info: { id: "child-c", parentID: "root-a" },
      },
    },
  });
  await plugin.event({ event: { type: "session.created", properties: { sessionID: "root-b" } } });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-b", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestState)).toEqual(["working", "idle", undefined, "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-a", undefined, "root-b", "root-b"]);
});

test("does not reclaim a deleted root from events before a new root is created", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-a", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.deleted", properties: { sessionID: "root-a" } } });
  await plugin["chat.message"]({ sessionID: "outside-chat" });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "outside-status", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.created", properties: { sessionID: "root-b" } } });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-b", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestState)).toEqual(["working", "idle", undefined, "working"]);
  expect(requests.map(requestSessionID)).toEqual(["root-a", undefined, "root-b", "root-b"]);
});

test("does not treat a normal run session argument as attached ownership", async () => {
  process.argv = ["node", "opencode", "run", "--session", "child-session"];
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent"]);
  expect(requests.map(requestState)).toEqual(["working"]);
  expect(requests.map(requestSessionID)).toEqual([undefined]);
});

test("tracks an attached child session as the pane session", async () => {
  process.argv = ["node", "opencode", "attach", "http://example.test", "--session", "child-session"];
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child-session" } } });

  expect(requests.map(requestMethod)).toEqual([
    "pane.report_agent_session",
    "pane.report_agent",
    "pane.report_agent",
  ]);
  expect(requests.map(requestState)).toEqual([undefined, "working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual([
    "child-session",
    "child-session",
    "child-session",
  ]);
});

test("recognizes an attached child session with equals syntax", async () => {
  process.argv = ["node", "opencode", "attach", "http://example.test", "--session=child-session"];
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-session",
        info: { id: "child-session", parentID: "root-session" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });

  expect(requests.map(requestSessionID)).toEqual(["child-session", "child-session"]);
});

test("reattaches a child session before its creation event", async () => {
  process.argv = ["node", "opencode", "attach", "http://example.test", "--session", "child-session"];
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-session", status: { type: "busy" } },
    },
  });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child-session" } } });

  expect(requests.map(requestMethod)).toEqual(["pane.report_agent", "pane.report_agent"]);
  expect(requests.map(requestState)).toEqual(["working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual(["child-session", "child-session"]);
});

test("prioritizes blocked and working states across multiple children", async () => {
  const plugin = await loadPlugin();

  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "root-session", status: { type: "idle" } },
    },
  });
  for (const childSessionID of ["child-a", "child-b"]) {
    await plugin.event({
      event: {
        type: "session.created",
        properties: {
          sessionID: childSessionID,
          info: { id: childSessionID, parentID: "root-session" },
        },
      },
    });
  }
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-a", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-b", status: { type: "blocked" } },
    },
  });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child-b" } } });
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child-a" } } });

  expect(requests.map(requestState)).toEqual(["idle", "working", "blocked", "working", "idle"]);
  expect(requests.map(requestSessionID)).toEqual([
    "root-session",
    undefined,
    undefined,
    undefined,
    undefined,
  ]);
});

test("ignores sessions outside an attached child tree", async () => {
  process.argv = ["node", "opencode", "attach", "http://example.test", "--session", "child-c"];
  const plugin = await loadPlugin();

  for (const [sessionID, type] of [
    ["parent-p", "session.status"],
    ["parent-p", "session.idle"],
    ["other-session", "session.status"],
    ["other-session", "session.idle"],
    ["parent-p", "session.updated"],
  ]) {
    await plugin.event({
      event: {
        type,
        properties:
          type === "session.status"
            ? { sessionID, status: { type: "busy" } }
            : { sessionID },
      },
    });
  }

  expect(requests).toHaveLength(0);

  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-c",
        info: { id: "child-c", parentID: "parent-p" },
      },
    },
  });
  await plugin.event({
    event: {
      type: "session.status",
      properties: { sessionID: "child-c", status: { type: "busy" } },
    },
  });
  await plugin.event({
    event: {
      type: "session.created",
      properties: {
        sessionID: "child-d",
        info: { id: "child-d", parentID: "child-c" },
      },
    },
  });
  await plugin.event({ event: { type: "permission.asked", properties: { sessionID: "child-d" } } });

  expect(requests.map(requestMethod)).toEqual([
    "pane.report_agent_session",
    "pane.report_agent",
    "pane.report_agent",
  ]);
  expect(requests.map(requestState)).toEqual([undefined, "working", "blocked"]);
  expect(requests.map(requestSessionID)).toEqual(["child-c", "child-c", undefined]);
});

function requestMethod(request: unknown): unknown {
  return isRecord(request) ? request.method : undefined;
}

function requestState(request: unknown): unknown {
  return requestParam(request, "state");
}

function requestSeq(request: unknown): unknown {
  return requestParam(request, "seq");
}

function requestSessionID(request: unknown): unknown {
  return requestParam(request, "agent_session_id");
}

function requestParam(request: unknown, name: string): unknown {
  if (!isRecord(request) || !isRecord(request.params)) {
    return undefined;
  }
  return request.params[name];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
