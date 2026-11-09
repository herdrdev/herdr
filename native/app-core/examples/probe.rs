//! Live checks against a server: the snapshot parses, and where a scroll must be addressed.
//! `cargo run -p shepherd-app-core --example probe -- /path/to/shepherd-client.sock`
use shepherd_app_core::endpoint::{Endpoint, Geometry, SNAPSHOT};
use shepherd_app_core::snapshot::Snapshot;
use shepherd_app_core::wire::{ClientKeyCode, ClientMouseKind, Incoming, PaneSurface};

fn main() -> std::io::Result<()> {
    let socket = std::env::args().nth(1).expect("socket path");
    let mut endpoint = Endpoint::connect(
        socket.as_ref(),
        Geometry {
            cols: 100,
            rows: 30,
            cell_width_px: 9,
            cell_height_px: 18,
        },
    )?;
    let mut sender = endpoint.sender()?;
    let mut surface: Option<PaneSurface> = None;
    let mut snapshot = None;
    while surface.is_none() || snapshot.is_none() {
        match endpoint.read()? {
            Incoming::Surface(s) => surface = Some(*s),
            Incoming::Control { kind, data } if kind == SNAPSHOT => {
                snapshot = Some(Snapshot::parse(&data).expect("snapshot parses"))
            }
            _ => {}
        }
    }
    let snapshot = snapshot.unwrap();
    println!(
        "snapshot r{}: {} space(s), {} tab(s), {} pane(s), {} agent(s)",
        snapshot.revision,
        snapshot.workspaces.len(),
        snapshot.tabs.len(),
        snapshot.panes.len(),
        snapshot.agents.len()
    );
    let pane = surface
        .as_ref()
        .unwrap()
        .panes
        .iter()
        .find(|p| p.focused)
        .expect("focused pane")
        .clone();
    println!(
        "pane {} rect {:?} inner {:?}",
        pane.pane_id, pane.rect, pane.inner_rect
    );
    // scrollback to scroll through
    sender.send_text(&pane.pane_id, "seq 1 300")?;
    sender.send_key(&pane.pane_id, ClientKeyCode::Enter, 0)?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    let offset = |endpoint: &mut Endpoint, surface: &mut PaneSurface| -> u64 {
        while let Ok(Some(message)) = endpoint.read_for(std::time::Duration::from_millis(400)) {
            match message {
                Incoming::Surface(s) => *surface = *s,
                Incoming::Patch(p) => {
                    let _ = surface.apply(*p);
                }
                _ => {}
            }
        }
        surface
            .panes
            .iter()
            .find(|p| p.pane_id == "w1:p1")
            .and_then(|p| p.scroll)
            .map(|s| s.offset_from_bottom)
            .unwrap_or(0)
    };
    let mut current = surface.unwrap();
    let before = offset(&mut endpoint, &mut current);
    sender.send_mouse(&pane.pane_id, ClientMouseKind::ScrollUp, 5, 5, 0, 3)?;
    let after = offset(&mut endpoint, &mut current);
    println!("scroll offset: {before} -> {after} (pane-relative cell (5,5), 3 lines)");
    Ok(())
}
