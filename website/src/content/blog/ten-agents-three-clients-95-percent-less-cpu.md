---
title: Ten agents, three clients, 95% less CPU
description: Herdr stopped animating its sidebar, drawing hidden panes, and repainting unchanged frames during mouse movement. The busiest sessions gained the most.
date: 2026-08-03
ogImage: /assets/og-blog-frames-v1.png
draft: false
---

In workloads dominated by unnecessary rendering, total CPU for the Herdr server and its attached clients fell by 89 to 95 percent. The renderer still runs at the same speed. Herdr now asks it to draw fewer frames.

The savings grow with the session. With one agent and one client, total CPU fell 91 percent. With ten agents producing background output and three clients, it fell 95 percent. Every frame the server skips is also one less frame to serialize, transmit, and apply in each client.

We found all three changes without a profiler. The first started as a complaint and gave us a rule: motion should mean something changed.

## The spinner was a complaint before it was a cost

Herdr's job is to tell you which agent needs you. You run five, ten, twenty of them, and the sidebar is how you know where to look.

So every working agent got a spinner. It seemed obvious at the time. Motion means alive, and an agent that is thinking should look like it is thinking.

Run twenty agents and you get twenty things moving at once, all saying the same thing, none saying it is the one that needs you. The workspace list already carried state as coloured dots. The sidebar was the noisy half of the same information. People told us. We agreed.

The first fix isolated spinner redraws to the status rectangle. Instead of rebuilding pane contents for every tick, the server reused the client's retained frame, patched the smaller result, and sent it. We had made the spinner about as cheap as possible. Then we removed it anyway. It was a design decision first; the CPU saving was a side effect.

<figure class="pf">
  <div class="pf-2">
    <div class="pf-panel">
      <div class="pf-label"><span>0.7.5</span><b>3 spinners, 8fps</b></div>
      <div class="pf-row working"><span class="pf-mark pf-spin"><i>⠋</i><i>⠙</i><i>⠹</i><i>⠸</i></span><div><strong>herdr</strong><small>working</small></div></div>
      <div class="pf-row working"><span class="pf-mark pf-spin"><i>⠋</i><i>⠙</i><i>⠹</i><i>⠸</i></span><div><strong>web-dashboard</strong><small>working</small></div></div>
      <div class="pf-row blocked"><span class="pf-mark">◉</span><div><strong>llm-proxy</strong><small>blocked, needs you</small></div></div>
      <div class="pf-row working"><span class="pf-mark pf-spin"><i>⠋</i><i>⠙</i><i>⠹</i><i>⠸</i></span><div><strong>data-pipeline</strong><small>working</small></div></div>
      <div class="pf-row done"><span class="pf-mark">●</span><div><strong>docs</strong><small>done</small></div></div>
    </div>
    <div class="pf-panel">
      <div class="pf-label"><span>0.8.0</span><b>nothing moves</b></div>
      <div class="pf-row working"><span class="pf-mark">●</span><div><strong>herdr</strong><small>working</small></div></div>
      <div class="pf-row working"><span class="pf-mark">●</span><div><strong>web-dashboard</strong><small>working</small></div></div>
      <div class="pf-row blocked"><span class="pf-mark">◉</span><div><strong>llm-proxy</strong><small>blocked, needs you</small></div></div>
      <div class="pf-row working"><span class="pf-mark">●</span><div><strong>data-pipeline</strong><small>working</small></div></div>
      <div class="pf-row done"><span class="pf-mark">●</span><div><strong>docs</strong><small>done</small></div></div>
    </div>
  </div>
  <figcaption class="pf-cap">Both panels carry the same information. Only one of them lets you find the blocked agent without reading. The left panel is animating in your browser right now, which is the entire argument.</figcaption>
</figure>

Working state is now a static coloured mark. Entering the state still renders immediately, so a change catches your eye when it happens. Remaining in the state schedules no animation.

That design change saved more CPU than we expected. The spinner woke the server on a timer, rendered the status surface, compared and prepared the frame, serialized it, sent it to every attached client, and made each client apply it. This happened roughly eight times a second for as long as any agent was working, even when the rest of the screen stayed unchanged.

Removing the spinner removed all of it: the timer, the animation deadlines, the scans asking whether any workspace contained a working pane, the animation-only render causes, the frame patching. Master produces zero scheduled animation frames. One agent working with nothing else on screen went from 1.467 percent CPU to 0.133 on Linux, and from 3.280 to 0.265 on macOS.

## The same rule, applied to output you cannot see

The same rule applies to output from panes nobody can see.

When a pane produced output, the PTY reader set a shared flag to say something had happened. That coalesced wakeups efficiently but discarded the source pane. By the time the server picked up the flag, it could not tell whether the pane was on screen. A background agent writing to a tab nobody was viewing still produced frames for every attached client.

<figure class="pf">
  <div class="pf-2">
    <div class="pf-panel">
      <div class="pf-label"><span>0.7.5</span><b>a pane writes</b></div>
      <div class="pf-flow">
        <div class="pf-step"><b>dirty flag set</b><s>something changed, we do not know what</s></div>
        <div class="pf-arrow">│</div>
        <div class="pf-step"><b>render</b><s>no way to rule it out</s></div>
        <div class="pf-arrow">│</div>
        <div class="pf-step"><b>serialize, transmit</b><s>once per attached client</s></div>
        <div class="pf-arrow">│</div>
        <div class="pf-step pf-cost"><b>3 clients apply a frame</b><s>for a tab none of them are showing</s></div>
      </div>
    </div>
    <div class="pf-panel">
      <div class="pf-label"><span>0.8.0</span><b>a pane writes</b></div>
      <div class="pf-flow">
        <div class="pf-step"><b>request_pty(pane 7)</b><s>the source is kept</s></div>
        <div class="pf-arrow">│</div>
        <div class="pf-step"><b>can any client see pane 7?</b><s>tabs, zoom, popups, direct attaches</s></div>
        <div class="pf-fork">
          <div class="pf-step pf-stop"><em>├─ no</em><b>stop here</b><s>bytes already parsed, nothing drawn</s></div>
          <div class="pf-step"><em>└─ yes</em><b>render as before</b><s>visible output is still visible output</s></div>
        </div>
      </div>
    </div>
  </div>
  <figcaption class="pf-cap">Render requests now carry their source. Writes from one pane coalesce into a single entry, while simultaneous writes from different panes each keep their identity.</figcaption>
</figure>

Skipping the render does not drop output. The bytes have already been parsed, so terminal state, scrollback, cursor, OSC metadata and agent detection all stay current. Switching to that tab renders from the current state. Where the mapping is unknown, Herdr treats the pane as visible, because a wrong skip leaves a stale screen while a wrong render only costs some CPU.

## Mouse movement should not redraw an unchanged frame

Every mouse event reported that the view had changed. Pointer movement over a pane is forwarded to the application inside it, but Herdr's own frame is usually identical before and after. At 60 motion events per second, that meant roughly 60 server renders and 60 client frames per second, to draw the same picture sixty times.

Herdr now tracks which of its own modes respond to hover: the global menu, context menus and the navigator. Outside those modes, movement alone repaints nothing. The events still reach the pane exactly as before.

The benchmark counted both ends. In every measured round, on both builds, 1,680 motion packets were sent and all 1,680 arrived in the pane. Version 0.7.5 emitted around 56 to 60 frames per second doing it. Master emitted none. Linux CPU fell from 9.450 percent to 0.667.

## Why three changes became ninety percent

Skipping one server render also skips frame preparation, serialization, transmission, and frame handling in every attached client.

More clients multiply the saving. Ten silent working panes with one client improved 78 percent. The same panes with three clients improved 91 percent.

<figure class="pf">
  <div class="pf-panel">
    <div class="pf-label"><span>total CPU, server plus every attached client</span><b>linux</b></div>
    <div class="pf-bars">
      <div class="pf-bar-row">
        <div class="pf-bar-name">one silent working pane</div>
        <div class="pf-bar old"><span>0.7.5</span><i style="width:11.3%"></i><em>1.467%</em></div>
        <div class="pf-bar new"><span>0.8.0</span><i style="width:1%"></i><em>0.133%</em></div>
      </div>
      <div class="pf-bar-row">
        <div class="pf-bar-name">ten silent working panes, three clients</div>
        <div class="pf-bar old"><span>0.7.5</span><i style="width:33.2%"></i><em>4.316%</em></div>
        <div class="pf-bar new"><span>0.8.0</span><i style="width:3.1%"></i><em>0.400%</em></div>
      </div>
      <div class="pf-bar-row">
        <div class="pf-bar-name">ten panes, nine hidden writers, three clients</div>
        <div class="pf-bar old"><span>0.7.5</span><i style="width:100%"></i><em>13.014%</em></div>
        <div class="pf-bar new"><span>0.8.0</span><i style="width:4.7%"></i><em>0.617%</em></div>
      </div>
      <div class="pf-bar-row">
        <div class="pf-bar-name">passive mouse motion at 60Hz</div>
        <div class="pf-bar old"><span>0.7.5</span><i style="width:72.6%"></i><em>9.450%</em></div>
        <div class="pf-bar new"><span>0.8.0</span><i style="width:5.1%"></i><em>0.667%</em></div>
      </div>
      <div class="pf-bar-row">
        <div class="pf-bar-name">forty-nine hidden writers at 60Hz</div>
        <div class="pf-bar old"><span>0.7.5</span><i style="width:44.6%"></i><em>5.800%</em></div>
        <div class="pf-bar new"><span>0.8.0</span><i style="width:34.1%"></i><em>4.433%</em></div>
      </div>
    </div>
  </div>
  <figcaption class="pf-cap">macOS followed the same shape: 22.958 percent to 1.673 in the hidden-output case, and 14.857 to 13.498 with forty-nine hidden writers at 60Hz.</figcaption>
</figure>

On 0.7.5, adding hidden background output to a session cost 8.7 CPU points on Linux and 12.7 on macOS. On master the same activity costs 0.2 and 0.6, which is roughly the price of reading and parsing the bytes and nothing else.

## Where it does not help

Fifty panes, forty-nine of them writing at 60Hz, is about 2,940 terminal updates per second. Every one of them still has to be read from the PTY and parsed into terminal state, because that is what makes the pane correct when you switch to it. Suppressing frames cannot suppress terminal emulation.

That case improved 23.6 percent on Linux and 9.1 on macOS. Visible output at 60Hz, in a pane you are watching, improved 33.9 percent on Linux and 2.7 on macOS. Idle sat at the measurement floor on both builds and deserves no percentage claim at all.

The optimization targets unnecessary rendering. Output-heavy cases remain dominated by PTY reads and terminal parsing.

## How this was measured

We compared official 0.7.5 release binaries with hashed master builds on separate Linux and macOS machines, giving 54 isolated observations per operating system. Every run used a fresh named session, real attached clients in fixed 86 by 47 terminals, a five-second warmup and twenty-one-second samples. The figures show total CPU for the Herdr server plus every attached client.

We paired every headline CPU result with a behavioural counter: scheduled frames per second for the sidebar, hidden-source skips for background output, and exact packet delivery counts for mouse motion. A CPU number on its own can be explained by a dozen things. A CPU number with a matching mechanism counter is harder to argue with.

This compares a shipped release against master, so intervening changes and build environments are part of the comparison. The profiler evidence matches the mechanisms closely, but not every CPU point can be assigned to one commit.

The work started with people saying the sidebar was too busy, along with bug reports and performance reports from people running far stranger sessions than we test. Keep them coming, and thank you to everyone helping make Herdr better.
