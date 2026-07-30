// installed by herdr
// managed by herdr; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// HERDR_INTEGRATION_ID=opencode
// HERDR_INTEGRATION_VERSION=9

import net from "node:net";

const SOURCE = "herdr:opencode";
const AGENT = "opencode";
let reportSeq = Date.now() * 1000;
let requestChain = Promise.resolve();
let reportedRootSessionID;

// Track the owning root's session tree so child state never leaks across roots.
const sessionParents = new Map();
const childStates = new Map();
let ownedRootSessionID;
let rootState = "idle";
const CHILD_EVENT_STATES = new Map([
  ["permission.asked", "blocked"],
  ["question.asked", "blocked"],
  ["permission.replied", "working"],
  ["question.replied", "working"],
  ["question.rejected", "working"],
  ["tool.execute.before", "working"],
  ["tool.execute.after", "working"],
  ["session.compacted", "working"],
  ["session.error", "blocked"],
]);

function nextReportSeq() {
  reportSeq += 1;
  return reportSeq;
}

function sessionIDFromProperties(properties) {
  return typeof properties?.sessionID === "string" && properties.sessionID
    ? properties.sessionID
    : undefined;
}

const SESSION_STATE_BY_STATUS = new Map([
  ["idle", "idle"],
  ["active", "working"],
  ["blocked", "blocked"],
  ["busy", "working"],
  ["pending", "working"],
  ["retry", "working"],
  ["running", "working"],
  ["streaming", "working"],
  ["working", "working"],
]);

function stateFromSessionStatus(status) {
  const kind = typeof status === "string" ? status : status?.type;
  return typeof kind === "string"
    ? SESSION_STATE_BY_STATUS.get(kind.toLowerCase())
    : undefined;
}

function attachedSessionIDFromArgv(argv) {
  // `process.argv[2]` is the OpenCode subcommand. Do not treat an arbitrary
  // argument named "attach" as one, such as a normal session id or URL.
  if (argv[2] !== "attach") {
    return undefined;
  }

  for (let index = 3; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--session") {
      return argv[index + 1] || undefined;
    }
    if (argument.startsWith("--session=")) {
      return argument.slice("--session=".length) || undefined;
    }
  }
  return undefined;
}

function childAggregateState() {
  let hasWorking = rootState === "working";
  if (rootState === "blocked") {
    return "blocked";
  }
  for (const state of childStates.values()) {
    if (state === "blocked") {
      return "blocked";
    }
    if (state === "working") {
      hasWorking = true;
    }
  }
  return hasWorking ? "working" : rootState;
}

function rememberSessionParent(properties) {
  const info = properties?.info;
  if (!info?.id) {
    return;
  }
  if (info.parentID) {
    sessionParents.set(info.id, info.parentID);
  } else {
    sessionParents.delete(info.id);
  }
}

function belongsToOwnedTree(sessionID) {
  const visited = new Set();
  let currentSessionID = sessionID;
  while (currentSessionID && !visited.has(currentSessionID)) {
    if (currentSessionID === ownedRootSessionID) {
      return true;
    }
    visited.add(currentSessionID);
    currentSessionID = sessionParents.get(currentSessionID);
  }
  return false;
}

function rootSessionIDFor(sessionID) {
  const visited = new Set();
  let rootSessionID = sessionID;
  let parentSessionID = sessionParents.get(rootSessionID);
  while (parentSessionID && !visited.has(parentSessionID)) {
    visited.add(rootSessionID);
    rootSessionID = parentSessionID;
    parentSessionID = sessionParents.get(rootSessionID);
  }
  return rootSessionID;
}

function setOwnedRootSession(sessionID) {
  if (sessionID === ownedRootSessionID) {
    return;
  }
  ownedRootSessionID = sessionID;
  childStates.clear();
  rootState = "idle";
}

function removeSessionSubtree(sessionID) {
  const removedSessionIDs = new Set([sessionID]);
  let foundDescendant = true;
  while (foundDescendant) {
    foundDescendant = false;
    for (const [childSessionID, parentSessionID] of sessionParents) {
      if (removedSessionIDs.has(parentSessionID) && !removedSessionIDs.has(childSessionID)) {
        removedSessionIDs.add(childSessionID);
        foundDescendant = true;
      }
    }
  }
  for (const removedSessionID of removedSessionIDs) {
    childStates.delete(removedSessionID);
    sessionParents.delete(removedSessionID);
  }
}

function resetOwnedRootSession() {
  if (ownedRootSessionID) {
    removeSessionSubtree(ownedRootSessionID);
  }
  childStates.clear();
  ownedRootSessionID = undefined;
  reportedRootSessionID = undefined;
  rootState = "idle";
}

function request(method, params) {
  const pending = requestChain.then(() => requestOnce(method, params));
  requestChain = pending.catch(() => {});
  return pending;
}

function requestOnce(method, params) {
  const paneId = process.env.HERDR_PANE_ID;
  const socketPath = process.env.HERDR_SOCKET_PATH;

  if (!paneId || !socketPath) {
    return Promise.resolve();
  }

  const socketEndpoint =
    process.platform === "win32" ? `\\\\.\\pipe\\${socketPath}` : socketPath;

  const requestId = `${SOURCE}:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
    .toString()
    .padStart(6, "0")}`;
  const request = {
    id: requestId,
    method,
    params: {
      pane_id: paneId,
      source: SOURCE,
      agent: AGENT,
      seq: nextReportSeq(),
      ...params,
    },
  };

  return new Promise((resolve) => {
    const client = net.createConnection(socketEndpoint, () => {
      client.write(`${JSON.stringify(request)}\n`);
    });

    const finish = () => {
      client.destroy();
      resolve();
    };

    client.setTimeout(500, finish);
    client.on("data", finish);
    client.on("error", finish);
    client.on("end", finish);
    client.on("close", resolve);
  });
}

function reportSession(sessionID, sessionStartSource) {
  if (!sessionID) {
    return Promise.resolve();
  }
  const params = { agent_session_id: sessionID };
  if (sessionStartSource) {
    params.session_start_source = sessionStartSource;
  }
  return request("pane.report_agent_session", params);
}

function reportState(state, sessionID) {
  const params = { state };
  if (sessionID) {
    reportedRootSessionID = sessionID;
    params.agent_session_id = sessionID;
  }
  return request("pane.report_agent", params);
}

export const HerdrAgentStatePlugin = async () => {
  if (
    process.env.HERDR_ENV !== "1" ||
    !process.env.HERDR_SOCKET_PATH ||
    !process.env.HERDR_PANE_ID
  ) {
    return {};
  }

  const attachedSessionID = attachedSessionIDFromArgv(process.argv);
  if (attachedSessionID) {
    setOwnedRootSession(attachedSessionID);
  }
  const sessionRole = (sessionID, canReplaceOwnedRoot = false) => {
    if (!sessionID) {
      return undefined;
    }
    if (attachedSessionID) {
      return belongsToOwnedTree(sessionID)
        ? sessionID === ownedRootSessionID
          ? "root"
          : "child"
        : undefined;
    }
    if (!ownedRootSessionID) {
      setOwnedRootSession(rootSessionIDFor(sessionID));
    } else if (!belongsToOwnedTree(sessionID)) {
      // Once a root is owned, only a new session.created event may take over.
      // Stale status, idle, deletion, or update events from a prior top-level
      // session must not reclaim ownership.
      if (sessionParents.has(sessionID) || !canReplaceOwnedRoot) {
        return undefined;
      }
      setOwnedRootSession(sessionID);
    }
    return sessionID === ownedRootSessionID ? "root" : "child";
  };
  const reportChildState = async (sessionID, state) => {
    childStates.set(sessionID, state);
    const aggregateState = childAggregateState();
    if (aggregateState) {
      await reportState(aggregateState);
    }
  };
  const reportRootState = async (state, sessionID) => {
    rootState = state;
    await reportState(childAggregateState(), sessionID);
  };

  return {
    "chat.message": async ({ sessionID }) => {
      const role = sessionRole(sessionID);
      if (sessionID && !role) {
        return;
      }
      if (role === "child") {
        await reportChildState(sessionID, "working");
        return;
      }
      await reportRootState("working", sessionID);
    },
    event: async ({ event }) => {
      const type = event?.type;
      const properties = event?.properties ?? {};
      const sessionID = sessionIDFromProperties(properties);
      rememberSessionParent(properties);
      const role = sessionRole(sessionID, type === "session.created");
      const isForeignTopLevelUpdate =
        type === "session.updated" &&
        !attachedSessionID &&
        sessionID &&
        !role &&
        !sessionParents.has(sessionID);

      if (sessionID && !role && !isForeignTopLevelUpdate) {
        return;
      }
      if (role === "child") {
        const state = CHILD_EVENT_STATES.get(type);
        if (state) {
          await reportChildState(sessionID, state);
          return;
        }
        if (type === "session.status") {
          const statusState = stateFromSessionStatus(properties.status);
          if (statusState) {
            await reportChildState(sessionID, statusState);
          }
          return;
        }
        if (type === "session.idle") {
          await reportChildState(sessionID, "idle");
          return;
        }
        if (type === "session.deleted") {
          removeSessionSubtree(sessionID);
          const aggregateState = childAggregateState();
          if (aggregateState) {
            await reportState(aggregateState);
          }
        }
        return;
      }

      switch (type) {
        case "session.created":
          // A root session.created is a genuine new-session start (subagent
          // creates are dropped above). Signal it so herdr replaces the pane's
          // prior session id instead of treating the change as cross-talk.
          await reportSession(sessionID, "new");
          break;
        case "session.updated":
          if (isForeignTopLevelUpdate || (sessionID && sessionID !== reportedRootSessionID)) {
            await reportSession(sessionID);
          }
          break;
        case "session.status": {
          const state = stateFromSessionStatus(properties.status);
          if (state) {
            await reportRootState(state, sessionID);
          } else {
            await reportSession(sessionID);
          }
          break;
        }
        case "tool.execute.before":
        case "tool.execute.after":
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
        case "session.compacted":
          await reportRootState("working", sessionID);
          break;
        case "permission.asked":
        case "question.asked":
        case "session.error":
          await reportRootState("blocked", sessionID);
          break;
        case "session.idle":
          await reportRootState("idle", sessionID);
          break;
        case "session.deleted":
          if (role === "root") {
            resetOwnedRootSession();
            await reportState("idle");
          }
          break;
        default:
          break;
      }
    },
  };
};
