---
title: Coding agents are becoming runtimes
description: Coding agent CLIs are becoming developer infrastructure. Project instructions, lifecycle state, hooks, and terminal runtimes need shared contracts.
date: 2026-06-10
draft: false
---

Coding agents have crossed an important line.

They started as chat interfaces that could edit code. Now they live inside repositories, terminals, editors, CI jobs, remote machines, mobile shells, multiplexers, and custom harnesses. They read project instructions. They run commands. They ask for permission. They spawn subagents. They pause, resume, compact, fail, recover, and keep working while the developer does something else.

That shape is familiar.

**This is runtime behavior.**

The model can be proprietary. The hosted API can be proprietary. The subscription can be priced however the lab needs to price it. That part belongs to the companies building the models.

But the CLI that runs inside a developer's terminal is part of the developer environment. It sits next to shells, language servers, package managers, test runners, terminals, editors, and multiplexers. Developers will compose it with the rest of their tools whether vendors plan for that or not.

**That is where the industry needs to be more careful.**

## We have seen this drift before

Software already has too many places where the same idea fractured into incompatible files and conventions.

JavaScript has `package.json`, then layers of package-manager lockfiles and workspace formats. Rust has `Cargo.toml`, which is refreshingly predictable. Python has lived through `requirements.txt`, `setup.py`, `setup.cfg`, `Pipfile`, `poetry.lock`, `pyproject.toml`, and tool-specific sections inside the same TOML file. Editors have their own settings. CI systems have their own YAML. Formatters and linters each add another little contract.

Some of that is natural. Tools have different jobs.

But the cost is real. Every repository becomes a map of overlapping conventions. Every new tool needs to learn which file matters, which one wins, and which one is legacy but still active.

Agent CLIs are young enough that we can avoid repeating the worst version of that.

Project instructions are the clearest example. [AGENTS.md](https://agents.md/) is trying to give coding agents one predictable place to read repository guidance. It describes itself as "a simple, open format for guiding coding agents" and says it is used by more than 60,000 open-source projects.

The shape is simple:

```text
one repository
one shared instruction file
many coding agents
```

Claude Code still stays on [`CLAUDE.md`, not `AGENTS.md`](https://docs.anthropic.com/en/docs/claude-code/memory#agents-md). The official workaround is to create a `CLAUDE.md` that imports `AGENTS.md`, or to symlink one file to the other.

That workaround works, and it is how standards drift begins.

First there is one extra file. Then every agent has a native file. Then every repository grows a compatibility layer. Then teams spend time keeping duplicated intent in sync.

Nobody loses a production database because a repo has two instruction files. The cost is smaller and more constant: duplicated intent, stale guidance, and another convention every tool has to learn.

## The terminal is the shared surface

There is another debate happening around access and harnesses.

Coding agents made per-token pricing feel very different. A chat session can be expensive. A coding agent can run tools, read files, loop, spawn work, and burn through a lot more context. So the labs started selling subscriptions with usage limits: some explicit, some soft, some hourly, some weekly, some hard to reason about from the outside.

Then developers started doing what developers always do. They put the model behind the harness they preferred.

That is where the Claude ecosystem became a useful case study. People used Claude subscriptions through third-party harnesses such as OpenClaw and OpenCode. Anthropic pushed back. Today Anthropic documents that [Claude Code can use Pro and Max subscriptions](https://support.claude.com/en/articles/11145838-use-claude-code-with-your-pro-or-max-plan), while [Claude paid plans and API access are separate products](https://support.claude.com/en/articles/9876003-i-have-a-paid-claude-subscription-pro-max-team-or-enterprise-plans-why-do-i-have-to-pay-separately-to-use-the-claude-api-and-console). Anthropic also introduced a separate [Agent SDK credit](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan) for programmatic and third-party Agent SDK usage. Zed summarized the practical effect well: first-party Claude usage and third-party agent or SDK usage now draw from different pools.

Anthropic can make that decision. They own the API. They own the subscription terms. Compute is expensive, and a first-party interactive CLI has different economics from arbitrary programmatic harnesses running through an SDK or protocol adapter.

The developer-tooling question remains.

> What does a developer actually own: the subscription, the API spend, the model access, or the runtime where the agent works?

When an agent runs in a terminal, it is no longer only a hosted product experience. It is a process in a developer environment. Developers will put that process in Herdr, Zed, tmux, SSH sessions, CI jobs, containers, and scripts. They will supervise it. They will route it. They will build workflows around it.

**The terminal is where ownership gets shared.**

That does not mean vendors lose the right to build first-party experiences. It means the CLI surface needs stable contracts because it has become infrastructure other tools depend on.

## Hooks are a symptom

Hooks show the same tension.

Most serious agent CLIs have hooks, plugins, or extensions now. That is good. The problem is that "hooks" can mean almost anything.

Some hooks are policy gates. Some are notifications. Some modify tool calls. Some inject context. Some observe messages. Some run at session boundaries. Some run around tools but miss interruptions, permission resolutions, or the exact state an external supervisor needs.

This matters because agent work is no longer a single request and response. A coding agent has a lifecycle.

For external tools, the minimum lifecycle is small:

```text
working -> actively doing work
blocked -> needs permission, input, recovery, or attention
idle    -> ready for the next instruction
```

The concrete shape could be small:

```json
{
  "session_id": "abc123",
  "state": "blocked",
  "reason": "permission_request",
  "source": "agent"
}
```

Permission prompts, failed tools, interrupted generations, automated reviews, subagent handoffs, and waiting for the user all need to land somewhere coherent.

Claude Code has a serious hook system, and Codex has its own shape too. The problem is not that these tools do nothing. The problem is that each agent defines its own boundary. A hook surface can be powerful and still fail to provide a portable lifecycle contract.

OpenCode is closer to the shape orchestration tools need. Its [plugin documentation](https://opencode.ai/docs/plugins/) exposes session-level events such as `session.idle`, `session.status`, `session.error`, `permission.asked`, and `permission.replied`.

pi, meanwhile, treats extensibility as a broader system. Its [extension documentation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md) describes TypeScript extensions that can subscribe to lifecycle events, register tools, add commands, intercept behavior, render UI, persist state, and hot-reload from local extension directories.

Those designs are worth praising because they treat the agent as something other tools can understand.

## What Herdr sees from below

**Herdr sits below the agent.**

It owns terminal panes. It keeps processes alive. It lets developers switch between agents, inspect them, recover sessions, and operate multiple agent processes as one workspace.

From that layer, runtime drift becomes very visible.

Herdr's first useful agent detection path was screen reading. If an agent drew "working", "waiting for approval", or "done" in the terminal, Herdr could read the terminal state and classify the pane.

That matched what humans saw, which made it useful. It also broke when agent UIs changed. If the fix lived inside the Herdr binary, users needed a full Herdr update just to recognize a new prompt or status line.

Another tempting signal was terminal activity.

```text
screen changing  -> probably working
screen still     -> maybe idle or blocked
```

Real agents break that quickly. A spinner can redraw while the meaningful state is blocked. A permission prompt can sit still while it is the most important thing in the pane. A subagent can produce activity while the parent session is waiting.

Terminal activity is evidence. Lifecycle is stronger.

Herdr now uses better signals where agents expose them, keeps screen evidence as a fallback, and ships hot-reloadable detection manifests so compatibility fixes do not always require a new Herdr binary. That is the practical bridge for today's ecosystem. The durable contract should come from agents themselves.

## Runtime support should be visible

Herdr can support agents in two broad ways.

Some agents expose reliable lifecycle signals. Those can have verified lifecycle support.

Other agents require observed detection through terminal output, screen evidence, manifests, and heuristics. That can work well, but it depends on UI behavior staying recognizable.

Herdr will publish this as a per-agent support matrix: which agents expose verified lifecycle signals, and which agents require observed detection.

**This distinction should be public because it helps everyone.**

Users get honest expectations. Agent authors get a target. Tool builders get a vocabulary. The ecosystem rewards agents that expose their runtime state cleanly.

The goal is not to force every agent to use the same plugin system. The goal is to standardize the parts that should never have been product differentiation.

## The ask

**Standardize the boring parts.**

Use one predictable project instruction format. Expose a small lifecycle contract. Make permission state observable. Make interruption state observable. Tie state to the session that is actually running. Document what external tools can rely on.

Keep competing on models, UX, planning, context management, speed, pricing, safety, and taste.

Do not compete on whether a repository needs five instruction files.

Do not compete on whether an external tool can tell if an agent is blocked.

Do not make every terminal runtime reverse-engineer every other terminal runtime.

Coding agents are becoming infrastructure. The companies building them still own their products, but developers own their environments.

Herdr is built from that position. The terminal is where these agents actually run. The runtime surface belongs to the developer workflow, and vendors share that space whether they design for it or not.

The better future is simple: agents should make their basic contracts visible, durable, and boring.

Then the rest of us can build.
