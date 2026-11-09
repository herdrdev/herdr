//! Connects to a running server, prints the first surface as text and the pane list.
//! `cargo run -p shepherd-app-core --example attach -- /path/to/shepherd-client.sock`
use shepherd_app_core::endpoint::{surface_text, Endpoint, Geometry};
use shepherd_app_core::wire::Incoming;
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let socket: PathBuf = std::env::args()
        .nth(1)
        .map(Into::into)
        .expect("socket path");
    let mut endpoint = Endpoint::connect(
        &socket,
        Geometry {
            cols: 100,
            rows: 30,
            cell_width_px: 9,
            cell_height_px: 18,
        },
    )?;
    println!("welcome: {}", endpoint.welcome["server_version"]);
    let start = std::time::Instant::now();
    loop {
        match endpoint.read()? {
            Incoming::Surface(surface) => {
                println!(
                    "surface {}x{} after {:?}, {} pane(s)",
                    surface.frame.width,
                    surface.frame.height,
                    start.elapsed(),
                    surface.panes.len()
                );
                for pane in &surface.panes {
                    println!(
                        "  {} focused={} rect={:?}",
                        pane.pane_id, pane.focused, pane.rect
                    );
                }
                println!("{}", surface_text(&surface.frame));
                return Ok(());
            }
            Incoming::Control { kind, data } => println!("control {kind} ({} bytes)", data.len()),
            other => println!("other {other:?}"),
        }
    }
}
