//! Types a command into the focused pane and times how long the result takes to come back.
//! `cargo run -p shepherd-app-core --example latency -- /path/to/shepherd-client.sock`
use shepherd_app_core::endpoint::{surface_text, Endpoint, Geometry};
use shepherd_app_core::wire::{ClientKeyCode, Incoming, PaneSurface};
use std::time::Instant;

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
    let mut surface: Option<PaneSurface>;
    let pane = loop {
        if let Incoming::Surface(s) = endpoint.read()? {
            let focused = s
                .panes
                .iter()
                .find(|p| p.focused)
                .map(|p| p.pane_id.clone());
            surface = Some(*s);
            if let Some(id) = focused {
                break id;
            }
        }
    };
    let mut times = Vec::new();
    for round in 0..8 {
        let marker = format!("ping-{round}-{}", std::process::id());
        let started = Instant::now();
        sender.send_text(&pane, &format!("echo {marker}"))?;
        sender.send_key(&pane, ClientKeyCode::Enter, 0)?;
        let mut updates = 0;
        loop {
            match endpoint.read()? {
                Incoming::Surface(s) => surface = Some(*s),
                Incoming::Patch(p) => {
                    if let Some(s) = surface.as_mut() {
                        if s.apply(*p).is_err() {
                            panic!("patch did not continue from the held surface");
                        }
                    }
                }
                _ => continue,
            }
            updates += 1;
            let text = surface
                .as_ref()
                .map(|s| surface_text(&s.frame))
                .unwrap_or_default();
            if text.lines().any(|l| l.trim() == marker) {
                println!(
                    "round {round}: {:?} from typing to the output on screen ({updates} updates)",
                    started.elapsed()
                );
                times.push(started.elapsed());
                break;
            }
        }
    }
    times.sort();
    println!(
        "median {:?}, worst {:?}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    Ok(())
}
