//! Bounded semantic migration gates. No opaque IDs, timing, or render scheduling
//! decisions enter this oracle. Keep the same runner for old/candidate captures.
use super::*;

struct Harness {
    pane: PaneTerminal,
    tx: mpsc::Sender<Bytes>,
    width: u16,
    height: u16,
    effects: Effects,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Effects {
    replies: Vec<u8>,
    clipboard: Vec<Vec<u8>>,
    bells: u32,
    cwd: Vec<std::path::PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    geometry: (u16, u16),
    cells: Vec<CellData>,
    text_rows: Vec<crate::ghostty::ScreenTextRow>,
    links: Vec<((u16, u16), String, String)>,
    cursor: TerminalCursorState,
    input: InputState,
    visible: String,
    recent: TerminalReadSnapshot,
    detection: String,
    title: Option<String>,
}

impl Harness {
    fn new(width: u16, height: u16) -> Self {
        let (tx, _rx) = mpsc::channel(16);
        let terminal = crate::ghostty::Terminal::new(width, height, 256).unwrap();
        Self {
            pane: PaneTerminal::new(GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap()),
            tx,
            width,
            height,
            effects: Effects::default(),
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.write_from_process(0, bytes);
    }

    fn write_from_process(&mut self, shell_pid: u32, bytes: &[u8]) {
        let result = self
            .pane
            .process_pty_bytes(PaneId::from_raw(1), shell_pid, bytes, &self.tx);
        for reply in result.terminal_responses {
            self.effects.replies.extend_from_slice(&reply);
        }
        self.effects.clipboard.extend(result.clipboard_writes);
        self.effects.bells += u32::from(result.terminal_bells);
        self.effects.cwd.extend(result.reported_cwd);
    }

    fn resize(&mut self, width: u16, height: u16) {
        for reply in self.pane.resize(height, width, 8, 16) {
            self.effects.replies.extend_from_slice(&reply);
        }
        self.width = width;
        self.height = height;
    }

    fn full_cells(&self) -> Vec<CellData> {
        // Read the frame buffer, not TestBackend's diff output (which may skip
        // wide spacer cells). This is the independent full-render oracle.
        let backend = ratatui::backend::TestBackend::new(self.width, self.height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut cells = Vec::new();
        terminal
            .draw(|frame| {
                self.pane
                    .render(frame, Rect::new(0, 0, self.width, self.height), false);
                cells = frame
                    .buffer_mut()
                    .content
                    .iter()
                    .map(CellData::from_ratatui_cell)
                    .collect();
            })
            .unwrap();
        cells
    }

    fn cursor(&self) -> Option<TerminalCursorState> {
        // Compare terminal semantics, not Windows' time-based cursor settling policy.
        current_cursor_state(&mut self.pane.ghostty.core.lock().unwrap())
    }

    fn observe(&self) -> Observation {
        let (_, cols, text_rows) = self.pane.screen_text_snapshot().unwrap();
        assert_eq!(cols, self.width);
        Observation {
            geometry: (self.width, self.height),
            cells: self.full_cells(),
            text_rows,
            links: self
                .pane
                .visible_hyperlinks(Rect::new(0, 0, self.width, self.height)),
            cursor: self.cursor().unwrap(),
            input: self.pane.input_state().unwrap(),
            visible: self.pane.visible_text(),
            recent: self.pane.recent_text_snapshot(32),
            detection: self.pane.detection_text(),
            title: self.pane.terminal_title(),
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn primary_screen_replay_honors_ed3_for_droid_at_chunk_boundaries() {
    // A zero PID would bypass the former process-specific filter entirely.
    // Use the real foreground-job lookup, with cleanup even on assertion failure.
    struct ChildGuard(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new("bash");
    command.args(["-c", "exec -a droid sleep 999"]);
    let child = ChildGuard(pair.slave.spawn_command(command).unwrap());
    let pid = child.0.process_id().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ready = crate::detect::foreground_job(pid).is_some_and(|job| {
            job.processes
                .iter()
                .any(|process| process.cmdline.as_deref() == Some("droid 999"))
        });
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "Droid foreground job not ready");
        std::thread::sleep(Duration::from_millis(10));
    }

    for clear in ["\x1b[3J", "\x1b[?3J"] {
        let prefix = format!("\x1b[?2026h\x1b[2J{clear}\x1b[H");
        let frame = |label: &str| {
            let mut bytes = format!("{prefix}welcome\r\n");
            for row in 0..55 {
                bytes.push_str(&format!("{label}-{row:02}\r\n"));
            }
            bytes.push_str("\x1b[?2026l");
            bytes
        };
        let old = frame("old");
        let new = frame("new");
        // Whole writes catch the old filter; bytewise writes and every split
        // within the erase prefix prove that PTY chunking cannot change ED3.
        for chunk_size in [usize::MAX, 1, 7] {
            for split in 0..=prefix.len() {
                let mut harness = Harness::new(80, 24);
                for bytes in old.as_bytes().chunks(chunk_size) {
                    harness.write_from_process(pid, bytes);
                }
                assert!(harness
                    .pane
                    .recent_text_snapshot(256)
                    .text
                    .contains("old-00"));
                harness.write_from_process(pid, &new.as_bytes()[..split]);
                for bytes in new.as_bytes()[split..].chunks(chunk_size) {
                    harness.write_from_process(pid, bytes);
                }
                let recent = harness.pane.recent_text_snapshot(256).text;
                assert!(recent.contains("new-54"), "redraw must complete");
                assert_eq!(
                    recent.matches("welcome").count(),
                    1,
                    "{clear:?}, split {split}, chunk {chunk_size}: {recent}"
                );
                assert!(!recent.contains("old-"), "{recent}");
                assert!(harness.pane.visible_text().contains("new-54"));
                harness.pane.scroll_up(256);
                let top = harness.pane.visible_text();
                assert!(top.contains("welcome") && top.contains("new-00"), "{top}");
                assert!(!top.contains("old-"), "{top}");
            }
        }
    }
}

#[test]
fn erase_display_preserves_screen_and_history_boundaries() {
    for clear in [b"\x1b[3J".as_slice(), b"\x1b[?3J"] {
        let mut harness = Harness::new(80, 24);
        for row in 0..55 {
            harness.write(format!("history-{row:02}\r\n").as_bytes());
        }
        assert!(harness
            .pane
            .recent_text_snapshot(256)
            .text
            .contains("history-00"));
        let primary = harness.pane.recent_text_snapshot(256);
        let visible = harness.pane.visible_text();

        harness.write(b"\x1b[?1049h\x1b[2J\x1b[Halternate");
        harness.write(clear);
        assert!(harness.pane.visible_text().contains("alternate"));
        harness.write(b"\x1b[?1049l");
        assert_eq!(harness.pane.recent_text_snapshot(256), primary);
        assert_eq!(harness.pane.visible_text(), visible);

        // ED2 clears only the display, not prior shell output in scrollback.
        harness.write(b"\x1b[2J\x1b[Hprompt");
        assert_eq!(harness.pane.visible_text().trim(), "prompt");
        assert!(harness
            .pane
            .recent_text_snapshot(256)
            .text
            .contains("history-00"));
        harness.write(clear);
        // ED3 clears history without erasing the current display.
        assert_eq!(harness.pane.visible_text().trim(), "prompt");
        assert_eq!(harness.pane.recent_text_snapshot(256).text.trim(), "prompt");
    }
}

#[test]
fn short_streams_are_invariant_at_every_byte_boundary() {
    let fixtures: &[&[u8]] = &[
        "a界e\u{301}🇯🇵!".as_bytes(),
        b"a\x1b[31;1mB\x1b[0m\x1b[2;3HZ\x1b[6n\x1b[?2004h",
        b"\x1b]8;;https://example.test/a\x1b\\link\x1b]8;;\x1b\\!",
        b"\x1b]52;c;aGk=\x07\x1b]2;migration\x1b\\\x07",
        b"\x1bP+q5463\x1b\\\x1bP+q6E6F7065\x9c\x1b[6n",
    ];
    for bytes in fixtures {
        let mut whole = Harness::new(16, 4);
        whole.write(bytes);
        let expected = whole.observe();
        for split in 0..=bytes.len() {
            let mut fragmented = Harness::new(16, 4);
            fragmented.write(&bytes[..split]);
            fragmented.write(&bytes[split..]);
            assert_eq!(
                fragmented.observe(),
                expected,
                "split {split}, bytes {bytes:?}"
            );
            assert_eq!(
                fragmented.effects, whole.effects,
                "effects at split {split}"
            );
        }
        let mut bytewise = Harness::new(16, 4);
        for byte in bytes.iter() {
            bytewise.write(std::slice::from_ref(byte));
        }
        assert_eq!(bytewise.observe(), expected);
        assert_eq!(bytewise.effects, whole.effects);
    }
}

const MIXED: &str = "ab\x1b[1;38;2;12;34;56m界e\u{301}\x1b[0m\x1b]8;;https://example.test/reflow\x1b\\🇯🇵xyz\x1b]8;;\x1b\\\r\nnext カタカナ end";

#[test]
fn mixed_reflow_reads_are_stable_and_chunk_independent() {
    let mut whole = Harness::new(12, 5);
    let mut fragmented = Harness::new(12, 5);
    whole.write(MIXED.as_bytes());
    for chunk in MIXED.as_bytes().chunks(3) {
        fragmented.write(chunk);
    }
    for (width, height) in [(12, 5), (8, 4), (17, 6), (9, 5), (12, 5)] {
        whole.resize(width, height);
        fragmented.resize(width, height);
        let expected = whole.observe();
        assert_eq!(whole.observe(), expected, "reads must not mutate semantics");
        assert_eq!(fragmented.observe(), expected);
        assert!(
            !expected.links.is_empty(),
            "fixture must retain a visible link"
        );
    }
}

#[test]
fn incremental_rows_reconstruct_full_render() {
    let mut incremental = Harness::new(12, 5);
    let mut full = Harness::new(12, 5);
    let mut retained = vec![CellData::from_ratatui_cell(&ratatui::buffer::Cell::default()); 60];
    // Independent terminals: full render must not consume incremental dirty state.
    for bytes in [
        b"".as_slice(),
        "ab\x1b[1;31m界e\u{301}\x1b[0m\r\nnext".as_bytes(),
        b"\x1b[2;2HZ",
        b"\x1b[1;1HA\x1b[5;8HB",
        b"\x1b]4;1;rgb:12/34/56\x07",
        b"\x1b[3;4H",
        b"\x1b[?1049hALT",
        b"\x1b[?1049l",
        // Blank cells must match the full renderer under every color source.
        b"\x1b[3;1H\x1b[44m\x1b[2K\x1b[0m",
        b"\x1b[4;2H\x1b[7m \x1b[0m \x1b[8m \x1b[0m",
        b"\x1b[?5h",
        b"\x1b[?5l",
        b"\x1b[5;1H\x1b]133;A\x07$ \x1b]133;B\x07  \x1b]133;C\x07",
        b"\x1b[5;1H\r\n\r\nscrolled\r\n",
    ] {
        incremental.write(bytes);
        full.write(bytes);
        match incremental.pane.collect_dirty_patch(12, 5) {
            TerminalDirtyPatchOutcome::Clean => {}
            TerminalDirtyPatchOutcome::Patch(patch) => {
                for (row, cells) in patch.rows {
                    assert_eq!(cells.len(), 12);
                    let start = usize::from(row) * 12;
                    retained[start..start + 12].clone_from_slice(&cells);
                }
            }
            TerminalDirtyPatchOutcome::Fallback => {
                panic!("bounded text fixture unexpectedly fell back")
            }
        }
        assert_eq!(retained, full.full_cells(), "after {bytes:?}");
        assert_eq!(incremental.cursor(), full.cursor());
    }
}

#[test]
fn sparse_dirty_patches_preserve_coordinates_and_clipped_rows() {
    for height in [3, 6] {
        let mut terminal = Harness::new(8, 6);
        terminal.write(b"\x1b[2;3H");
        terminal.pane.collect_dirty_patch(8, 6);
        assert!(matches!(
            terminal.pane.collect_dirty_patch(8, 6),
            TerminalDirtyPatchOutcome::Clean
        ));
        terminal.write(b"\x1b[2;3HX\x1b[5;4HY");
        let TerminalDirtyPatchOutcome::Patch(patch) = terminal.pane.collect_dirty_patch(8, height)
        else {
            panic!("expected sparse patch");
        };
        let expected_rows = if height == 3 { vec![1] } else { vec![1, 4] };
        assert_eq!(
            patch.rows.iter().map(|(y, _)| *y).collect::<Vec<_>>(),
            expected_rows
        );
        assert!(patch.rows.iter().all(|(_, cells)| cells.len() == 8));

        let core = terminal.pane.ghostty.core.lock().unwrap();
        let mut iterator = crate::ghostty::RowIterator::new().unwrap();
        let mut rows = core
            .render_state
            .populate_row_iterator(&mut iterator)
            .unwrap();
        let mut y = 0;
        while rows.next() {
            assert_eq!(rows.dirty().unwrap(), height == 3 && y == 4);
            y += 1;
        }
        assert_eq!(y, 6);
    }
}

#[test]
fn dirty_patch_fallback_keeps_previously_collected_rows_dirty() {
    let mut terminal = Harness::new(8, 6);
    terminal.write(b"\x1b[2;3H");
    terminal.pane.collect_dirty_patch(8, 6);
    terminal.write(b"\x1b[2;3HX\x1b[5;4H\x1b]8;;https://example.test\x1b\\Y\x1b]8;;\x1b\\");
    assert!(matches!(
        terminal.pane.collect_dirty_patch(8, 6),
        TerminalDirtyPatchOutcome::Fallback
    ));
    let core = terminal.pane.ghostty.core.lock().unwrap();
    let mut iterator = crate::ghostty::RowIterator::new().unwrap();
    let mut rows = core
        .render_state
        .populate_row_iterator(&mut iterator)
        .unwrap();
    let mut dirty = Vec::new();
    let mut y = 0;
    while rows.next() {
        if rows.dirty().unwrap() {
            dirty.push(y);
        }
        y += 1;
    }
    assert_eq!(dirty, vec![1, 4]);
}

#[test]
fn complete_history_replay_supports_plain_append() {
    // ANSI history is text restoration, not a parser/cursor snapshot. End at a
    // non-wrapping printable cell with SGR and OSC8 closed; no pending tab/CSI.
    let mut source = Harness::new(24, 4);
    source.write(b"\x1b[31mred\x1b[0m\r\nplain");
    let ansi = source.pane.recent_unwrapped_ansi(32);
    let mut restored = Harness::new(24, 4);
    restored.pane.ghostty.seed_history_ansi(&ansi);
    assert_eq!(
        restored.pane.recent_text_snapshot(32),
        source.pane.recent_text_snapshot(32)
    );
    // Establish the documented live-output boundary explicitly instead of
    // requiring history formatting to restore arbitrary cursor/SGR state.
    for terminal in [&mut source, &mut restored] {
        terminal.write(b"\x1b[0m\r\nappended");
    }
    assert_eq!(
        restored.pane.recent_text_snapshot(32),
        source.pane.recent_text_snapshot(32)
    );
    assert!(restored.effects.clipboard.is_empty());
    assert!(restored.effects.replies.is_empty());
}

#[test]
#[ignore = "non-gating release render scaling profile"]
fn render_scale_profile_sparse_dirty_patches() {
    for count in [1, 15] {
        let mut panes = (0..count).map(|_| Harness::new(80, 24)).collect::<Vec<_>>();
        for pane in &mut panes {
            for line in 0..40 {
                pane.write(format!("{line:04} populated 界 terminal row\r\n").as_bytes());
            }
            pane.write(b"\x1b[12;40H");
            pane.pane.collect_dirty_patch(80, 24);
        }
        for sparse in [false, true] {
            let mut samples = Vec::new();
            for sample in 0..35 {
                let mut elapsed = std::time::Duration::ZERO;
                for iteration in 0..20 {
                    if sparse {
                        let bytes: &[u8] = if iteration % 2 == 0 {
                            b"\x1b[12;40HX"
                        } else {
                            b"\x1b[12;40HY"
                        };
                        for pane in &panes {
                            pane.pane.ghostty.core.lock().unwrap().terminal.write(bytes);
                        }
                    }
                    let start = std::time::Instant::now();
                    for pane in &panes {
                        let outcome = pane.pane.collect_dirty_patch(80, 24);
                        if sparse {
                            let TerminalDirtyPatchOutcome::Patch(ref patch) = outcome else {
                                panic!("expected sparse patch");
                            };
                            assert_eq!(patch.rows.len(), 1);
                            assert_eq!(patch.rows[0].0, 11);
                        } else {
                            assert!(matches!(outcome, TerminalDirtyPatchOutcome::Clean));
                        }
                        std::hint::black_box(outcome);
                    }
                    elapsed += start.elapsed();
                }
                if sample >= 5 {
                    samples.push(elapsed.as_nanos() / 20);
                }
            }
            samples.sort_unstable();
            eprintln!(
                "dirty_patch_scale panes={count} sparse={sparse} median_ns={} p95_ns={}",
                samples[samples.len() / 2],
                samples[samples.len() * 95 / 100]
            );
        }
    }
}

#[test]
fn capture_bounded_migration_observations() {
    let mut terminal = Harness::new(12, 5);
    let mut observations = Vec::new();
    terminal.write(MIXED.as_bytes());
    observations.push(terminal.observe());
    for (width, height) in [(8, 4), (17, 6), (12, 5)] {
        terminal.resize(width, height);
        observations.push(terminal.observe());
    }
    terminal.write(b"\x1b[6n\x1b[?2004h\x1b]52;c;aGk=\x07\x07");
    observations.push(terminal.observe());
    terminal.pane.scroll_up(2);
    observations.push(terminal.observe());
    terminal.pane.scroll_reset();
    observations.push(terminal.observe());
    // An explicit path prevents fixtures from silently being regenerated.
    // Compare old/new with diff; explain each difference, never bulk-bless it.
    if let Some(path) = std::env::var_os("HERDR_MIGRATION_OBSERVATIONS") {
        std::fs::write(
            path,
            format!("{observations:#?}\n{:#?}\n", terminal.effects),
        )
        .unwrap();
    }
    assert_eq!(observations.last().unwrap(), &terminal.observe());
}
