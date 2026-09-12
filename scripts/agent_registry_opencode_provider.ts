// Loopback-only deterministic model fixture. No upstream client, credentials, or forwarding.
import assert from "node:assert/strict";

export type Mode = "COMPLETE" | "PERMISSION" | "CANCEL";
export type ProviderEvent = { at: string; type: string; marker?: string; [key: string]: unknown };
type Gate = { marker: string; mode: Mode; release: () => void; aborted: boolean; released: boolean };
const encoder = new TextEncoder();
const markerPattern = /HERDR_E2E_(COMPLETE|PERMISSION|CANCEL)_[a-zA-Z0-9_-]+/g;

function text(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((part) => text(part?.text)).join("\n");
  return "";
}

/** Route by the most recent marked user turn, not request order (auxiliary calls may race). */
function route(body: any): { marker: string; mode: Mode; toolResult: boolean } | undefined {
  const messages: any[] = Array.isArray(body.messages) ? body.messages : [];
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role !== "user") continue;
    const matches = [...text(messages[i].content).matchAll(markerPattern)];
    const match = matches.at(-1);
    if (match) return {
      marker: match[0], mode: match[1] as Mode,
      toolResult: messages.slice(i + 1).some((m) => m.role === "tool"),
    };
  }
}

export function createFixtureProvider(options: { port?: number; onEvent?: (event: ProviderEvent) => void } = {}) {
  const events: ProviderEvent[] = [];
  const gates = new Map<string, Gate>();
  const liveStreams = new Set<() => void>();
  const event = (type: string, details: Record<string, unknown> = {}) => {
    const item = { at: new Date().toISOString(), type, ...details };
    events.push(item);
    options.onEvent?.(item);
  };
  const server = Bun.serve({
    hostname: "127.0.0.1", port: options.port ?? 0, idleTimeout: 0,
    async fetch(request) {
      const url = new URL(request.url);
      if (url.pathname === "/control/status" && request.method === "GET") {
        return Response.json({ events, gates: [...gates.values()].map(({ marker, mode, aborted, released }) => ({ marker, mode, aborted, released })) });
      }
      if (url.pathname === "/control/release" && request.method === "POST") {
        const { marker } = await request.json() as { marker: string };
        const gate = gates.get(marker);
        if (!gate || gate.aborted || gate.released) return Response.json({ error: "no pending gate" }, { status: 409 });
        gate.released = true;
        event("released", { marker });
        gate.release();
        return Response.json({ released: marker });
      }
      if (url.pathname !== "/v1/chat/completions" || request.method !== "POST") {
        event("unexpected_endpoint", { path: url.pathname, method: request.method });
        return Response.json({ error: { message: "fixture only supports POST /v1/chat/completions" } }, { status: 404 });
      }
      const body = await request.json() as any;
      if (body.model !== "fixture" || body.stream !== true) {
        event("invalid_request", { body });
        return Response.json({ error: { message: "fixture requires model=fixture, stream=true" } }, { status: 400 });
      }
      const turn = route(body);
      event("request", { marker: turn?.marker, toolResult: turn?.toolResult, body });
      if (turn && !turn.toolResult && gates.has(turn.marker)) {
        event("duplicate_turn", { marker: turn.marker });
        return Response.json({ error: { message: "duplicate initial turn marker" } }, { status: 409 });
      }
      // Abort and cancel both unblock the writer. No heartbeat/text is emitted behind the gate.
      let closed = false;
      let gate: Gate | undefined;
      let unblock = () => {};
      const wait = new Promise<void>((resolve) => { unblock = resolve; });
      const abort = () => {
        if (closed) return;
        if (gate) gate.aborted = true;
        event("aborted", { marker: turn?.marker });
        closed = true;
        unblock();
        liveStreams.delete(abort);
      };
      request.signal.addEventListener("abort", abort, { once: true });
      if (turn && !turn.toolResult) {
        gate = { marker: turn.marker, mode: turn.mode, release: unblock, aborted: false, released: false };
        gates.set(turn.marker, gate);
      }
      liveStreams.add(abort);
      const stream = new ReadableStream<Uint8Array>({
        async start(controller) {
          const send = (delta: object, finish_reason: string | null = null) => {
            if (closed) return;
            controller.enqueue(encoder.encode(`data: ${JSON.stringify({
              id: "chatcmpl-herdr-fixture", object: "chat.completion.chunk", created: 1, model: "fixture",
              choices: [{ index: 0, delta, finish_reason }],
              ...(finish_reason ? { usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } } : {}),
            })}\n\n`));
          };
          try {
            send({ role: "assistant" });
            if (gate) {
              send({ content: "Local fixture is processing.\n" });
              event("gated", { marker: gate.marker, mode: gate.mode });
              await wait;
            }
            if (closed) return;
            if (turn?.mode === "PERMISSION" && !turn.toolResult) {
              // Use only the advertised tool. Fail closed if the installed schema changes.
              const tool = body.tools?.find((t: any) => t.type === "function" && t.function?.name === "bash");
              assert(tool, "installed OpenCode did not advertise bash");
              const args: Record<string, unknown> = { command: "printf 'HERDR_PERMISSION_PROBE\\n'" };
              for (const key of tool.function.parameters?.required ?? []) {
                if (key === "description") args.description = "Print a harmless local probe (reject this request)";
                else assert(key in args, `unsupported required bash argument: ${key}`);
              }
              send({ tool_calls: [{ index: 0, id: `call_${turn.marker}`, type: "function", function: { name: "bash", arguments: JSON.stringify(args) } }] });
              send({}, "tool_calls");
              event("tool_call", { marker: turn.marker, args });
            } else {
              send({ content: turn?.toolResult ? "Permission rejected; fixture complete." : "Fixture complete." });
              send({}, "stop");
            }
            controller.enqueue(encoder.encode("data: [DONE]\n\n"));
            closed = true;
            controller.close();
            event("finished", { marker: turn?.marker, toolResult: turn?.toolResult });
          } catch (error) {
            event("stream_error", { marker: turn?.marker, error: String(error) });
            closed = true;
            controller.error(error);
          } finally {
            request.signal.removeEventListener("abort", abort);
            liveStreams.delete(abort);
          }
        },
        cancel: abort,
      });
      return new Response(stream, { headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" } });
    },
  });
  return {
    url: `http://127.0.0.1:${server.port}`, events,
    async release(marker: string) {
      const response = await fetch(`http://127.0.0.1:${server.port}/control/release`, {
        method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ marker }),
      });
      assert(response.ok, `release ${marker}: ${await response.text()}`);
    },
    async stop() {
      for (const abort of liveStreams) abort();
      await server.stop(true);
    },
  };
}

/** Provider-only contract check: no Herdr/OpenCode processes or external requests. */
export async function selfTestProvider() {
  const provider = createFixtureProvider();
  const request = (marker: string, more: object[] = [], signal?: AbortSignal) => fetch(`${provider.url}/v1/chat/completions`, {
    method: "POST", headers: { "Content-Type": "application/json" }, signal,
    body: JSON.stringify({ model: "fixture", stream: true, messages: [{ role: "user", content: marker }, ...more],
      tools: [{ type: "function", function: { name: "bash", parameters: { required: ["command"] } } }] }),
  });
  try {
    // Independent concurrent markers prove that gates are not keyed by call count.
    const complete = "HERDR_E2E_COMPLETE_selftest";
    const permission = "HERDR_E2E_PERMISSION_selftest";
    const a = await request(complete);
    const b = await request(permission);
    const readA = a.text();
    const readB = b.text();
    await provider.release(permission);
    assert.match(await readB, /tool_calls/);
    assert(!provider.events.some((e) => e.type === "finished" && e.marker === complete));
    await provider.release(complete);
    assert.match(await readA, /\[DONE\]/);
    const followup = await request(permission, [{ role: "assistant", content: null, tool_calls: [] }, { role: "tool", tool_call_id: `call_${permission}`, content: "Rejected" }]);
    assert.match(await followup.text(), /Permission rejected/);
    const controller = new AbortController();
    const cancel = "HERDR_E2E_CANCEL_selftest";
    const response = await request(cancel, [], controller.signal);
    const reader = response.body!.getReader();
    await reader.read();
    controller.abort();
    await reader.cancel().catch(() => {});
    const deadline = Date.now() + 3000;
    while (!provider.events.some((e) => e.type === "aborted" && e.marker === cancel) && Date.now() < deadline) await Bun.sleep(20);
    assert(provider.events.some((e) => e.type === "aborted" && e.marker === cancel), "cancel closes stream");
    assert(!provider.events.some((e) => ["stream_error", "invalid_request", "duplicate_turn"].includes(e.type)));
    console.log("PASS: provider concurrent gates, tool result, completion and abort (loopback only)");
  } finally { await provider.stop(); }
}
