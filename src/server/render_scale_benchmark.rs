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
        let config = Config::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &config,
            AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = workspaces;
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_scrollbars = true;

        let mut client = ClientShellState::new(ClientShellConfig::from_config(&config));
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
    let mut pipeline = RenderPipeline::new(build(count));
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual client-rendered pipeline scaling profile"]
async fn render_scale_profile() {
    print_profiles("background workspaces (one pane each)", workspaces);
    print_snapshot_encoding_profiles("background workspaces", workspaces);
    print_profiles("active panes (one workspace)", active_panes);
    print_snapshot_encoding_profiles("active panes", active_panes);
}
