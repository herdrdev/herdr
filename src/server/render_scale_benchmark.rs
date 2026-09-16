use std::hint::black_box;
use std::time::{Duration, Instant};

use ratatui::layout::{Direction, Rect};

use crate::app::{App, AppPolicy};
use crate::client::{ClientShellConfig, ClientShellState};
use crate::config::Config;
use crate::kitty_graphics::HostCellSize;
use crate::protocol::PaneSurfaceFrame;
use crate::terminal::TerminalRuntime;
use crate::workspace::Workspace;

const COLS: u16 = 120;
const ROWS: u16 = 40;
const SAMPLE_COUNT: usize = 40;
const WARMUP_COUNT: usize = 5;
const CARDINALITIES: [usize; 3] = [1, 15, 50];
const CLIENT_CARDINALITIES: [usize; 2] = [1, 4];

#[derive(Clone, Copy)]
struct StageStats {
    median_us: u128,
    p95_us: u128,
    max_us: u128,
}

struct PipelineStats {
    server: StageStats,
    client: StageStats,
    total: StageStats,
}

struct RenderPipeline {
    app: App,
    client: ClientShellState,
    graphics_delivery: crate::kitty_graphics::surface::DeliveryCache,
}

impl RenderPipeline {
    fn new(workspaces: Vec<Workspace>) -> Self {
        Self::with_config(workspaces, &Config::default())
    }

    fn with_config(workspaces: Vec<Workspace>, config: &Config) -> Self {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            config,
            AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = workspaces;
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_scrollbars = true;

        let mut client = ClientShellState::new(ClientShellConfig::from_config(config));
        client.set_snapshot(Box::new(super::client_shell::snapshot(
            &app,
            "bench-boot",
            1,
            None,
            None,
        )));

        Self {
            app,
            client,
            graphics_delivery: crate::kitty_graphics::surface::DeliveryCache::default(),
        }
    }

    fn render_once(&mut self) -> (Duration, Duration) {
        let surface_size = self.client.surface_size(COLS, ROWS);
        let started = Instant::now();
        let target = self
            .app
            .state
            .active
            .map(|workspace_index| crate::ui::TabSurfaceTarget {
                workspace_index,
                tab_index: self.app.state.workspaces[workspace_index].active_tab_index(),
            });
        let rendered = super::client_shell::render_pane_surface(
            &mut self.app,
            target,
            Rect::new(0, 0, surface_size.cols, surface_size.rows),
            true,
            true,
            HostCellSize {
                width_px: 1,
                height_px: 1,
            },
            &self.graphics_delivery,
            1,
        );
        let server_elapsed = started.elapsed();
        self.graphics_delivery = rendered.graphics_delivery;

        let started = Instant::now();
        self.client.set_pane_surface(PaneSurfaceFrame {
            boot_id: "bench-boot".into(),
            projection_revision: 1,
            surface_revision: 0,
            frame: rendered.frame,
            panes: rendered.panes,
            splits: rendered.splits,
            popup: rendered.popup,
            graphics: rendered.graphics,
        });
        black_box(
            self.client
                .compose(COLS, ROWS)
                .expect("benchmark pipeline should compose a complete frame"),
        );
        let client_elapsed = started.elapsed();

        (server_elapsed, client_elapsed)
    }
}

fn history() -> String {
    (0..2_000).map(|line| format!("line-{line}\r\n")).collect()
}

fn runtime(history: &str) -> TerminalRuntime {
    TerminalRuntime::test_with_scrollback_bytes(COLS, ROWS, 1024 * 1024, history.as_bytes())
}

fn workspaces(workspace_count: usize) -> Vec<Workspace> {
    let history = history();
    (0..workspace_count)
        .map(|index| {
            let mut workspace = Workspace::test_new(&format!("bench-{}", index + 1));
            let root_pane = workspace.tabs[0].root_pane;
            workspace.insert_test_runtime(root_pane, runtime(&history));
            workspace
        })
        .collect()
}

fn active_panes(pane_count: usize) -> Vec<Workspace> {
    let history = history();
    let mut workspace = Workspace::test_new("bench");
    let root_pane = workspace.tabs[0].root_pane;
    workspace.insert_test_runtime(root_pane, runtime(&history));
    let mut pane_ids = vec![root_pane];

    for index in 1..pane_count {
        let target = pane_ids[(index - 1) / 2];
        workspace.tabs[0].layout.focus_pane(target);
        let direction = if index % 2 == 0 {
            Direction::Vertical
        } else {
            Direction::Horizontal
        };
        let pane_id = workspace.test_split(direction);
        workspace.insert_test_runtime(pane_id, runtime(&history));
        pane_ids.push(pane_id);
    }

    vec![workspace]
}

fn summarize(mut samples: Vec<Duration>) -> StageStats {
    samples.sort_unstable();
    StageStats {
        median_us: samples[SAMPLE_COUNT / 2].as_micros(),
        p95_us: samples[(SAMPLE_COUNT - 1) * 95 / 100].as_micros(),
        max_us: samples[SAMPLE_COUNT - 1].as_micros(),
    }
}

fn profile(build: fn(usize) -> Vec<Workspace>, count: usize) -> PipelineStats {
    profile_pipeline(RenderPipeline::new(build(count)))
}

fn profile_pipeline(mut pipeline: RenderPipeline) -> PipelineStats {
    for _ in 0..WARMUP_COUNT {
        black_box(pipeline.render_once());
    }

    let mut server = Vec::with_capacity(SAMPLE_COUNT);
    let mut client = Vec::with_capacity(SAMPLE_COUNT);
    let mut total = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let (server_elapsed, client_elapsed) = pipeline.render_once();
        server.push(server_elapsed);
        client.push(client_elapsed);
        total.push(server_elapsed + client_elapsed);
    }

    PipelineStats {
        server: summarize(server),
        client: summarize(client),
        total: summarize(total),
    }
}

fn print_stage(
    label: &str,
    rows: &[(usize, PipelineStats)],
    stage: fn(&PipelineStats) -> StageStats,
) {
    let baseline = stage(&rows[0].1);
    println!("  {label}");
    println!("       count  median_us  p95_us  max_us  median_vs_1x  p95_vs_1x");
    for (count, pipeline) in rows {
        let stats = stage(pipeline);
        println!(
            "  {count:>10}  {:>9}  {:>6}  {:>6}  {:>12.2}  {:>9.2}",
            stats.median_us,
            stats.p95_us,
            stats.max_us,
            stats.median_us as f64 / baseline.median_us.max(1) as f64,
            stats.p95_us as f64 / baseline.p95_us.max(1) as f64,
        );
    }
}

fn print_profiles(label: &str, build: fn(usize) -> Vec<Workspace>) {
    let rows = CARDINALITIES.map(|count| (count, profile(build, count)));
    println!("{label}");
    print_stage("server pane surface", &rows, |stats| stats.server);
    print_stage("client shell composition", &rows, |stats| stats.client);
    print_stage("combined pipeline", &rows, |stats| stats.total);
}

fn profile_snapshot_encoding(
    build: fn(usize) -> Vec<Workspace>,
    count: usize,
    client_count: usize,
) -> StageStats {
    let pipeline = RenderPipeline::new(build(count));
    let run = || {
        let started = Instant::now();
        let template = super::client_shell::snapshot(&pipeline.app, "bench-boot", 1, None, None);
        for client_index in 0..client_count {
            let mut snapshot = template.clone();
            snapshot.revision = client_index as u64 + 1;
            let message = crate::protocol::endpoint::snapshot_message(&snapshot)
                .expect("benchmark snapshot should serialize");
            black_box(
                bincode::serde::encode_to_vec(message, bincode::config::standard())
                    .expect("benchmark snapshot message should frame"),
            );
        }
        started.elapsed()
    };
    for _ in 0..WARMUP_COUNT {
        black_box(run());
    }
    summarize((0..SAMPLE_COUNT).map(|_| run()).collect())
}

fn print_snapshot_encoding_profiles(label: &str, build: fn(usize) -> Vec<Workspace>) {
    println!("{label} snapshot projection + JSON framing");
    println!("       panes  clients  median_us  p95_us  max_us");
    for count in CARDINALITIES {
        for client_count in CLIENT_CARDINALITIES {
            let stats = profile_snapshot_encoding(build, count, client_count);
            println!(
                "  {count:>10}  {client_count:>7}  {:>9}  {:>6}  {:>6}",
                stats.median_us, stats.p95_us, stats.max_us
            );
        }
    }
}

fn print_token_rule_profiles() {
    let rules = std::iter::repeat_n(
        "{ contains = 'NO-MATCH', ignore_case = true, bold = true }",
        15,
    )
    .chain(std::iter::once("{ starts_with = 'bench', bold = true }"))
    .collect::<Vec<_>>()
    .join(",");
    for (label, build) in [
        ("background", workspaces as fn(usize) -> Vec<Workspace>),
        ("active", active_panes),
    ] {
        for conditional in [false, true] {
            let config: Config = toml::from_str(&format!(
                "[ui.sidebar.agents]\nrows = [[{{ token = 'workspace', rules = [{}] }}]]\n[ui.sidebar.spaces]\nrows = [[{{ token = 'workspace', rules = [{}] }}]]",
                if conditional { &rules } else { "" }, if conditional { &rules } else { "" },
            )).unwrap();
            let rows = [1, 15].map(|count| {
                let mut pipeline = RenderPipeline::with_config(build(count), &config);
                pipeline.app.state.ensure_test_terminals();
                for terminal in pipeline.app.state.terminals.values_mut() {
                    terminal.detected_agent = Some(crate::detect::Agent::Pi);
                }
                pipeline
                    .client
                    .set_snapshot(Box::new(super::client_shell::snapshot(
                        &pipeline.app,
                        "bench-boot",
                        1,
                        None,
                        None,
                    )));
                (count, profile_pipeline(pipeline))
            });
            println!(
                "token rules {label}: populated agents, rules_per_token={}",
                if conditional { 16 } else { 0 }
            );
            print_stage("client shell composition", &rows, |stats| stats.client);
        }
    }
}

#[derive(Clone, Copy)]
enum SurfaceDamagePattern {
    Dense,
    Checkerboard,
}

impl SurfaceDamagePattern {
    fn name(self) -> &'static str {
        match self {
            Self::Dense => "dense",
            Self::Checkerboard => "checkerboard",
        }
    }

    fn apply(self, surface: &mut PaneSurfaceFrame, iteration: usize) {
        for (index, cell) in surface.frame.cells.iter_mut().enumerate() {
            cell.symbol = if matches!(self, Self::Checkerboard) && !index.is_multiple_of(2) {
                "c"
            } else if iteration.is_multiple_of(2) {
                "a"
            } else {
                "b"
            }
            .into();
        }
    }
}

struct SurfaceEncodingClient {
    state: super::render_stream::ClientRenderState,
    decoder: Option<crate::protocol::surface_reuse::Decoder>,
}

fn surface_encoding_client(mode: &str, surface: &PaneSurfaceFrame) -> SurfaceEncodingClient {
    let reuse = mode == "reuse";
    let delta = mode == "delta";
    let mut client = SurfaceEncodingClient {
        state: super::render_stream::ClientRenderState::new(
            crate::protocol::RenderEncoding::SemanticFrame,
        ),
        decoder: (reuse || delta).then(|| crate::protocol::surface_reuse::Decoder::new(delta)),
    };
    client.state.enable_surface_reuse(reuse);
    client.state.enable_surface_delta(delta);
    let initial = client
        .state
        .prepare_pane_surface(surface.clone())
        .expect("initial surface should be prepared");
    if let Some(decoder) = client.decoder.as_mut() {
        decoder.decode(initial.message().clone()).unwrap();
    }
    client.state.commit_sent_frame(initial);
    client
}

fn profile_surface_damage(
    surface: &PaneSurfaceFrame,
    pattern: SurfaceDamagePattern,
    mode: &str,
    client_count: usize,
) {
    let mut clients = (0..client_count)
        .map(|_| surface_encoding_client(mode, surface))
        .collect::<Vec<_>>();
    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    let mut bytes_per_update = 0usize;
    for iteration in 0..WARMUP_COUNT + SAMPLE_COUNT {
        let mut candidate = surface.clone();
        candidate.projection_revision = iteration as u64 + 2;
        pattern.apply(&mut candidate, iteration);
        if iteration > 0 && matches!(pattern, SurfaceDamagePattern::Checkerboard) {
            let previous = clients[0].state.last_pane_surface().unwrap();
            let changed = previous
                .frame
                .cells
                .iter()
                .zip(&candidate.frame.cells)
                .filter(|(old, new)| old != new)
                .count();
            assert_eq!(changed, candidate.frame.cells.len().div_ceil(2));
        }
        let started = Instant::now();
        bytes_per_update = 0;
        for client in &mut clients {
            let prepared = client
                .state
                .prepare_pane_surface(candidate.clone())
                .expect("damage should produce a surface update");
            let mut bytes = Vec::new();
            crate::protocol::write_message(&mut bytes, prepared.message()).unwrap();
            bytes_per_update += bytes.len();
            let decoded = crate::protocol::read_message(
                &mut bytes.as_slice(),
                crate::protocol::MAX_GRAPHICS_FRAME_SIZE,
            )
            .unwrap();
            black_box(if let Some(decoder) = client.decoder.as_mut() {
                decoder.decode(decoded).unwrap()
            } else {
                decoded
            });
            client.state.commit_sent_frame(prepared);
        }
        if iteration >= WARMUP_COUNT {
            samples.push(started.elapsed());
        }
    }
    let stats = summarize(samples);
    println!(
        "  {:<11}  {client_count:>7}  {mode:>7}  {:>9}  {:>12}",
        pattern.name(),
        stats.median_us,
        bytes_per_update
    );
}

fn print_surface_damage_profiles() {
    println!("surface dense/checkerboard encode + decode at {COLS}x{ROWS}");
    println!("  pattern       clients     mode  median_us  bytes/update");
    for pane_count in [1, 15] {
        let mut pipeline = RenderPipeline::new(active_panes(pane_count));
        pipeline.render_once();
        let rendered = super::client_shell::render_pane_surface(
            &mut pipeline.app,
            Some(crate::ui::TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 0,
            }),
            Rect::new(0, 0, COLS, ROWS),
            true,
            false,
            HostCellSize::default(),
            &pipeline.graphics_delivery,
            1,
        );
        let mut surface = PaneSurfaceFrame {
            boot_id: "bench-boot".into(),
            projection_revision: 1,
            surface_revision: 0,
            frame: rendered.frame,
            panes: rendered.panes,
            splits: rendered.splits,
            popup: rendered.popup,
            graphics: rendered.graphics,
        };
        // Normalize the baseline so dense and checkerboard iterations have stable damage.
        for cell in &mut surface.frame.cells {
            cell.symbol = "a".into();
        }
        println!("  active panes={pane_count}");
        for pattern in [
            SurfaceDamagePattern::Dense,
            SurfaceDamagePattern::Checkerboard,
        ] {
            for client_count in [1, 15] {
                for mode in ["legacy", "reuse", "delta"] {
                    profile_surface_damage(&surface, pattern, mode, client_count);
                }
            }
        }
    }
}

fn print_surface_reuse_profiles() {
    println!("surface encode + decode at {COLS}x{ROWS}");
    println!("  layout       panes  cells_changed  mode     median_us  bytes_per_update");
    for (label, build) in [
        ("background", workspaces as fn(usize) -> Vec<Workspace>),
        ("active", active_panes),
    ] {
        for count in [1, 15] {
            let mut pipeline = RenderPipeline::new(build(count));
            pipeline.render_once();
            let rendered = super::client_shell::render_pane_surface(
                &mut pipeline.app,
                Some(crate::ui::TabSurfaceTarget {
                    workspace_index: 0,
                    tab_index: 0,
                }),
                Rect::new(0, 0, COLS, ROWS),
                true,
                false,
                HostCellSize::default(),
                &pipeline.graphics_delivery,
                1,
            );
            let surface = PaneSurfaceFrame {
                boot_id: "bench-boot".into(),
                projection_revision: 1,
                surface_revision: 0,
                frame: rendered.frame,
                panes: rendered.panes,
                splits: rendered.splits,
                popup: rendered.popup,
                graphics: rendered.graphics,
            };
            for cells_changed in [false, true] {
                for mode in ["legacy", "reuse", "delta"] {
                    let mut client = surface_encoding_client(mode, &surface);
                    let mut samples = Vec::new();
                    let mut bytes_per_update = 0;
                    for index in 0..WARMUP_COUNT + SAMPLE_COUNT {
                        let mut candidate = surface.clone();
                        candidate.projection_revision = index as u64 + 2;
                        if cells_changed {
                            candidate.frame.cells[0].symbol =
                                if index % 2 == 0 { "a" } else { "b" }.into();
                        }
                        let started = Instant::now();
                        let prepared = client.state.prepare_pane_surface(candidate).unwrap();
                        let mut bytes = Vec::new();
                        crate::protocol::write_message(&mut bytes, prepared.message()).unwrap();
                        bytes_per_update = bytes.len();
                        let decoded = crate::protocol::read_message(
                            &mut bytes.as_slice(),
                            crate::protocol::MAX_FRAME_SIZE,
                        )
                        .unwrap();
                        black_box(if let Some(decoder) = client.decoder.as_mut() {
                            decoder.decode(decoded).unwrap()
                        } else {
                            decoded
                        });
                        client.state.commit_sent_frame(prepared);
                        if index >= WARMUP_COUNT {
                            samples.push(started.elapsed());
                        }
                    }
                    let stats = summarize(samples);
                    println!(
                    "  {label:<10}  {count:>5}  {cells_changed:>13}  {mode:>7}  {:>9}  {bytes_per_update:>16}",
                    stats.median_us
                );
                }
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual client-rendered pipeline scaling profile"]
async fn render_scale_profile() {
    print_profiles("background workspaces (one pane each)", workspaces);
    print_snapshot_encoding_profiles("background workspaces", workspaces);
    print_profiles("active panes (one workspace)", active_panes);
    print_snapshot_encoding_profiles("active panes", active_panes);
    print_token_rule_profiles();
    print_surface_reuse_profiles();
    print_surface_damage_profiles();
}
