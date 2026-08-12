---
title: Herdr is joining Y Combinator. The runtime stays open.
description: Herdr is joining Y Combinator. The runtime is Apache-2.0 and stays open, and here is what the funding is actually for.
date: 2026-08-06
draft: false
lede: false
ogImage: /assets/og-blog-yc-v1.png
---

Hey everyone, it's Can, the only person behind Herdr. This post is about how I decided to change that.

Four months ago I was looking for another job. Thinking about where to go career-wise. Thinking about software engineering and its future in general, my hobby projects, applications, whiteboard interviews (I still can't believe we do those). And at some point it hit me: I am the bottleneck.

This is a management and engineering problem. I don't want to install an app or learn a new tool just to manage my agents and my development style. I want something that fits me. You know how every company releases their own agent these days? No, don't do that. Let my agent integrate with your product, don't offer me another agent.

Same idea here. I didn't like what I tried, so why not build it? So I built a runtime. But what is a runtime?

## Runtime

CLI coding agents run in terminals, and terminals have always been our home: editing code (yes, nvim chads), running servers, navigating projects, maintaining CI, machines, configurations. It's the root. It's the connection. So agents need first-class primitives there.

A terminal pane belongs to an agent. A pane belongs to a tab. Tabs belong to a project.

They need to be persistent. We have agents running for hours now, sometimes days. And once you have the runtime, where they run stops mattering. **You should be able to run them anywhere and keep them running.**

That's where Herdr was born. But a runtime needs an interface, so I built the TUI.

## TUI

The TUI is the UI I use every day, and it carried Herdr this far because it shows what I believe Herdr should do: track agents at a glance, divide work between projects instead of getting lost in a pile of agents, and alert you only when an agent actually needs you.

Its biggest advantage, and the reason it stays first-class forever, is that it's bundled. Install Herdr on your VPS, ssh in, and your UI is already there. Or run `herdr --remote user@host` and it installs itself. Depending on your network speed, you're a couple of seconds away from an agent running. One command and you're in. Nothing beats that.

Still, the TUI was never meant to be the only client. A runtime means people can build on top of it, and people did! A [Raycast extension](https://x.com/vladscale/status/2080195067088871582), a [Stream Deck with buttons for herdr](https://x.com/timvdhoorn/status/2067907258260795808), an [iOS app](https://x.com/imnotchalk/status/2077647387414384936) that drives a whole session from a phone. More than 500 plugins, one month after the marketplace released. I didn't build any of these.

The TUI has its limits too. Some of the ideas in my head are hard to build inside a terminal, and I trust terminals, but Herdr will need more clients. More on that later.

## Where we are now

So a solo project reached **25k stars and 340k downloads**, and it became more than one person can carry.

Herdr is joining <span style="color:#fb651e">Y Combinator, F26 batch</span>. I'm excited to make Herdr a company that builds a developer tool for anyone juggling agents all day. I want to build a small team: people who keep the runtime healthy, reliable, fast, easy to run anywhere, and more extensible, so you can do more while Herdr stays small.

The runtime, what you use right now, stays free. Apache-2.0. That's why I recently switched it from AGPL to Apache: I want everyone to use Herdr freely.

## Where we're going

I want to build on top of the open Herdr runtime like everyone else. I'll keep supporting the open source while building the features people really need. The demand is already visible: multiple clients, a laptop, a VPS for the six-hour job, a sandbox for risky code and ephemeral agents. Herdr can run anywhere today, but those machines are disconnected. They should be connected.

There are many more features in my head, but I don't want to rush. People keep telling me they love that Herdr stays lean, and that's what I want to protect. In an age where adding one more feature costs nothing, choosing what goes in the core is the most important decision, I believe. So the core stays small, and everything else stays possible through extensions: your style, your flows, your company's setup, your themes, your crazy ideas.

Thank you for getting Herdr this far. Thanks for the support, for sharing the love. You have no idea how grateful I am.

Anyway, back to work.

![A ram, asleep above the clouds. Or working, hard to tell.](/assets/where-do-agents-run-while-you-sleep.png)
