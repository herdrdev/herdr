// Runs only in an owned test subprocess: real OS signals never reach bun:test.
const [modulePath, scenario] = process.argv.slice(2);
const handlers = new Map<string, (event: any, ctx?: any) => unknown>();
const { default: install } = await import(modulePath);
install({ on: (name, handler) => handlers.set(name, handler), events: { on() {} } });

let sessionPath = "/tmp/herdr-interrupted-a.jsonl";
const ctx = {
  hasUI: true,
  mode: "tui",
  isIdle: () => true,
  sessionManager: { getSessionFile: () => sessionPath, getSessionId: () => "test" },
};
let shuttingDown = false;
async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  // Match the native host's bounded shutdown contract.
  const deadline = setTimeout(() => process.exit(42), 2000);
  await handlers.get("session_shutdown")?.({ reason: "quit" }, ctx);
  clearTimeout(deadline);
  process.exit(0);
}
// Hosts register after loading extensions but before emitting session_start.
if (scenario !== "no-native-handler") {
  process.on("SIGTERM", shutdown);
  process.on("SIGHUP", shutdown);
}
await handlers.get("session_start")?.({ reason: "startup" }, ctx);
if (scenario === "reload") {
  await handlers.get("session_shutdown")?.({ reason: "reload" }, ctx);
  sessionPath = "/tmp/herdr-interrupted-b.jsonl";
  await handlers.get("session_start")?.({ reason: "reload" }, ctx);
}
if (scenario === "switch") {
  sessionPath = "/tmp/herdr-interrupted-b.jsonl";
  const select = handlers.get("session_switch") ?? handlers.get("session_start");
  await select?.({ reason: "resume" }, ctx);
}
process.send?.({ ready: true, termListeners: process.listenerCount("SIGTERM") });
if (scenario === "clean") await shutdown();
setInterval(() => {}, 1000);
