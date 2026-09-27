# Sidebar sections visual check

1. Set `[ui.sidebar] layout = "sections"` and `header = "sky"`, then restart Herdr.
2. Compare the expanded sidebar with `shot-d-normal.png` and `shot-d-light.png` from `/home/ubuntu2/.agents/briefs/mat-207/` in dark and light themes.
3. Check the four shelf order, two-line cards, project badges, status colors, and narrow-width field removal. Click both card lines and confirm they select the same tab. Counts read `Snoozed 2` and `Settled 14`, without parentheses.
4. Confirm AC9: open the `≡` checklist beside the bell and toggle its nine areas by mouse and keys. Open and operate it from Home, Inbox, Work, Usage, and the dock; modal owners keep control.
5. Confirm AC10: `header = "sky"` shows the sky band and `herdr` wordmark, with Search and the goto-key hint below and the View bar beneath them. With `header = "plain"`, the sky band and wordmark disappear and the remaining header controls reflow to the plain layout.
6. Check the host strip above the footer: the local host comes first and the help hint sits at the right. At 18 columns, the local host remains visible (truncated if needed) and the help hint yields first. Check the sidebar still keeps three tab-list rows at 80x24 with Notes, Pomodoro, and Hosts enabled.
7. Set `ui.nerd_font = false` and check the card and shelf icons.
8. Hide Notes, invoke the Toggle Notes action, then show Notes again. Confirm the focused pane still receives typing and the Notes editor does not.
