use super::*;

// Renderer reference: openai/codex 6b9826e3aa83b1a5947db50f4332cb9c65f1b340,
// codex-rs/tui/src/status_indicator_widget.rs. Capture date: 2026-09-14.
// Repro prompts: sleep 45 with exec_command/yield_time_ms=1000 then write_stdin;
// /compact; sleep 25 with require_escalated + auto_review; concurrent sleep 12
// and sleep 13 with the same approval settings. No real project was modified.
// Cropped, otherwise unchanged detection text from Codex 0.154.0, gpt-5.6-luna
// low, tui.animations=false and tui.raw_output_mode=false. Each crop retains the
// status block, composer and footer. The static OSC title was "codex".
const WAITING: &str = "\
Waiting for background terminal (36s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close
  └ sleep 45


› Ask Codex to do anything

  gpt-5.6-luna
";

const COMPACTING: &str = "\
Compacting context (1s • esc to interrupt)
  └ Making room to continue.


› Ask Codex to do anything

  gpt-5.6-luna
";

const APPROVAL: &str = "\
• Running sleep 25

Reviewing approval request (4s • esc to interrupt)
  └ /bin/bash -lc 'sleep 25'


› Ask Codex to do anything

  gpt-5.6-luna
";

const PARALLEL_APPROVALS: &str = "\
Reviewing 2 approval requests (4s • esc to interrupt)
  └ • /bin/bash -lc 'sleep 12'
    • /bin/bash -lc 'sleep 13'


› Ask Codex to do anything

  gpt-5.6-luna
";

fn assert_working(screen: &str) {
    let result = osc_explain(Agent::Codex, screen, "codex", "");
    assert_eq!(result.state, AgentState::Working, "{screen}");
    assert!(result.visible_working, "{screen}");
    assert!(!result.visible_idle, "{screen}");
}

#[test]
fn waiting_for_background_terminal_remains_working() {
    with_manifest_dirs("codex-live-waiting", || assert_working(WAITING));
}

#[test]
fn compaction_remains_working() {
    with_manifest_dirs("codex-live-compacting", || assert_working(COMPACTING));
}

#[test]
fn automatic_approval_review_remains_working() {
    with_manifest_dirs("codex-live-approval", || assert_working(APPROVAL));
}

#[test]
fn parallel_approval_reviews_remain_working() {
    with_manifest_dirs("codex-live-parallel-approvals", || {
        assert_working(PARALLEL_APPROVALS);
    });
}

#[test]
fn mcp_approval_and_unbound_interrupt_remain_working() {
    with_manifest_dirs("codex-live-mcp-and-unbound", || {
        // Further unchanged crops: a local stdio MCP wait tool with auto_review,
        // and sleep 40 with tui.keymap.chat.interrupt_turn=[].
        for screen in [
            "\
• Calling status_probe.wait({})

Reviewing approval request (6s • esc to interrupt)
  └ MCP wait on status_probe


› Ask Codex to do anything

  Working · gpt-5.6-luna
",
            "\
Working (4s) · 1 background terminal running · /ps to view · /stop to close


› Ask Codex to do anything

  gpt-5.6-luna
",
            "\
Waiting for background terminal (35s) · 1 background terminal running · /ps to view · /stop to close
  └ sleep 40


› Ask Codex to do anything

  gpt-5.6-luna
",
        ] {
            assert_working(screen);
        }
    });
}

#[test]
fn live_status_changes_do_not_emit_completion_sound() {
    with_manifest_dirs("codex-live-completion-sound", || {
        let mut previous = AgentState::Working;
        for screen in [WAITING, APPROVAL, PARALLEL_APPROVALS, COMPACTING] {
            let state = osc_explain(Agent::Codex, screen, "codex", "").state;
            assert_eq!(
                crate::app::actions::notification_sound_for_state_change(false, previous, state),
                None,
                "premature completion for {screen}"
            );
            previous = state;
        }
        let completed = "• READY\n\n› Ask Codex to do anything\n\n  gpt-5.6-luna\n";
        let state = osc_explain(Agent::Codex, completed, "codex", "").state;
        assert_eq!(state, AgentState::Idle);
        assert_eq!(
            crate::app::actions::notification_sound_for_state_change(false, previous, state),
            Some(crate::sound::Sound::Done)
        );
    });
}

#[test]
fn animated_status_and_model_output_can_have_identical_text() {
    with_manifest_dirs("codex-live-model-lookalikes", || {
        // Captured while Luna streamed a literal-output request. Master also
        // accepts these exact strings as animated status after #4099. Plain text
        // cannot distinguish them; rejecting these captures would reject genuine
        // animated status too. Retain the captures to document this ambiguity.
        for screen in [
            "\
• Reviewing approval request (4s • esc to interrupt)


› Ask Codex to do anything

  gpt-5.6-luna
",
            "\
• Reviewing approval request (4s • esc to interrupt)

  Reviewing 2 approval requests (4s • esc to interrupt)

  Compacting context (1s • esc to interrupt)

  Waiting for background terminal (36s • esc to interrupt).


› Ask Codex to do anything

  gpt-5.6-luna
",
        ] {
            let result = osc_explain(Agent::Codex, screen, "codex", "");
            assert_eq!(result.state, AgentState::Working, "{screen}");
            assert!(result.visible_working, "{screen}");
        }
    });
}

#[test]
fn reasoning_headers_and_interrupt_key_configuration_remain_working() {
    with_manifest_dirs("codex-live-status-shape", || {
        // Source contracts, not literal captures: streaming.rs derives the
        // header from the first bold reasoning summary, so labels are unbounded.
        // status_indicator_widget.rs formats the clock independently of the
        // configured key and renders just the clock when that key is unbound.
        for header in [
            "Checking the next step",
            "Working",
            "Compacting context",
            "Reconnecting... 2/5",
            "Booting MCP server: status_probe",
            "Starting MCP servers (0/2): one, two",
            "Setting up sandbox...",
        ] {
            for clock in [
                "14m 42s • esc to interrupt",
                "1h 02m 03s • esc to interrupt",
                "2s • f12 to interrupt",
                "36s",
            ] {
                let screen =
                    format!("{header} ({clock})\n\n› Ask Codex to do anything\n\n  gpt-5.6-luna\n");
                assert_working(&screen);
            }
        }
    });
}

#[test]
fn status_is_bound_to_the_composer_not_a_fixed_footer_line_count() {
    with_manifest_dirs("codex-live-status-composer", || {
        let screen = WAITING.replace(
            "› Ask Codex to do anything",
            "› unsent draft\n  spanning\n  several\n  lines",
        );
        assert_working(&screen);
        // An earlier interruption must not suppress a new, live status block.
        assert_working(&format!("■ Conversation interrupted\n\n{COMPACTING}"));
    });
}

#[test]
fn queued_input_does_not_hide_the_live_status() {
    with_manifest_dirs("codex-live-queued-input", || {
        // Unchanged crop after steering Luna during sleep 45, with interrupt
        // unbound. Pending input is rendered between the status and composer.
        let screen = "\
Working (14s) · 1 background terminal running · /ps to view · /stop to close

• Messages to be submitted after next tool call
  ↳ When the current sleep finishes, reply READY. Do not start any new tools.

› Ask Codex to do anything

  gpt-5.6-luna
";
        assert_working(screen);
        // An indented model continuation cannot open a status block.
        let output = format!("  {screen}");
        assert_eq!(
            osc_explain(Agent::Codex, &output, "codex", "").state,
            AgentState::Idle
        );
        // Other source-defined PendingInputPreview sections use the same layout.
        for header in [
            "Messages to be submitted after next tool call (press esc to interrupt and send immediately)",
            "Messages to be submitted at end of turn",
            "Queued follow-up inputs",
        ] {
            assert_working(&screen.replace("Messages to be submitted after next tool call", header));
        }
        // PendingThreadApprovals describes other threads, not this task's state.
        assert_working(&COMPACTING.replace(
            "\n\n›",
            "\n\n  ! Approval needed in Helper [explorer]\n    /subagents to switch threads\n\n›",
        ));
    });
}

#[test]
fn tool_rows_and_inactive_lookalikes_do_not_count_as_working() {
    with_manifest_dirs("codex-live-inactive-lookalikes", || {
        for screen in [
            "• Running sleep 12\n\n• READY\n\n› Ask Codex to do anything\n",
            "› Explain Compacting context (1s • esc to interrupt)\n\n  gpt-5.6-luna\n",
            "• READY\n\n› Ask Codex to do anything\n\n  Working\n",
            "Working\n\n› Ask Codex to do anything\n",
            // reconnect.rs keeps a timer after permanently failing to reconnect.
            "Reconnect failed — check the endpoint, then relaunch (5s)\n\n› draft\n",
            "• Example:\n\n    Compacting context (1s • esc to interrupt)\n\n› draft\n",
            "• Queued follow-up inputs\n  ↳ Working (2s)\n\n› draft\n",
        ] {
            let result = osc_explain(Agent::Codex, screen, "codex", "");
            assert_eq!(result.state, AgentState::Idle, "{screen}");
            assert!(!result.visible_working, "{screen}");
        }
    });
}

#[test]
fn real_user_approval_and_transcript_view_keep_their_precedence() {
    with_manifest_dirs("codex-live-status-precedence", || {
        let approval = format!("{APPROVAL}Press enter to confirm or esc to cancel\n");
        let result = osc_explain(Agent::Codex, &approval, "codex", "");
        assert_eq!(result.state, AgentState::Blocked);
        assert!(result.visible_blocker);
        assert!(!result.visible_working);

        let transcript = format!(
            "{WAITING}↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev\n"
        );
        let result = osc_explain(Agent::Codex, &transcript, "codex", "");
        assert_eq!(result.state, AgentState::Unknown);
        assert!(result.skip_state_update);
        assert!(!result.visible_working);
    });
}
