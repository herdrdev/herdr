use super::*;

fn scrolling_lines(range: std::ops::Range<usize>) -> Vec<u8> {
    range
        .map(|n| {
            format!(
                "build step {n:>3}: compiled module_{n} in {}ms\r\n",
                n * 7 % 97
            )
        })
        .collect::<String>()
        .into_bytes()
}

fn apply_rows(frame: &mut FrameData, rows: &[crate::protocol::PaneSurfacePatchRow]) {
    for row in rows {
        let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
        frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
    }
}

#[tokio::test]
async fn surface_scroll_sends_scrolling_output_as_a_shift_and_new_rows() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(&scrolling_lines(0..40));
    server
        .clients
        .get_mut(&1)
        .expect("scroll client")
        .render_state
        .enable_surface_scroll(true);
    server.render_and_stream();
    let mut decoder = protocol::surface_reuse::Decoder::new(false, true);
    let ServerMessage::PaneSurface(mut shell) = decoder
        .decode(read_server_message(
            render_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        ))
        .expect("initial surface")
    else {
        panic!("expected the initial pane surface");
    };

    write_shared_test_pane(&mut server, pane_id, &scrolling_lines(40..42));
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let bytes = render_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let message = read_server_message(bytes.clone());
    assert!(matches!(
        &message,
        ServerMessage::EndpointControl { kind, .. } if kind == protocol::surface_scroll::MESSAGE_KIND
    ));

    // The decoder hands the client shell an ordinary patch that reaches the
    // server's committed surface exactly.
    let ServerMessage::PaneSurfacePatch(patch) = decoder.decode(message).expect("scroll decode")
    else {
        panic!("expected an expanded pane patch");
    };
    apply_rows(&mut shell.frame, &patch.rows);
    let committed = server.clients[&1]
        .render_state
        .last_pane_surface()
        .expect("committed surface");
    assert_eq!(shell.frame.cells, committed.frame.cells);
    assert!(frame_text(&shell.frame).contains("module_41"));

    let expanded = HeadlessServer::frame_server_message(&ServerMessage::PaneSurfacePatch(patch))
        .expect("expanded frame");
    assert!(
        bytes.len() * 4 < expanded.len(),
        "scroll frame {} should be far smaller than the rows it replaces {}",
        bytes.len(),
        expanded.len()
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn surface_scroll_is_not_sent_to_a_peer_that_did_not_negotiate_it() {
    let (mut server, _control_rx, render_rx, pane_id) =
        retained_test_server_with_control(&scrolling_lines(0..40));
    server.render_and_stream();
    let _ = render_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    write_shared_test_pane(&mut server, pane_id, &scrolling_lines(40..42));
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    let message = read_server_message(render_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    assert!(matches!(message, ServerMessage::PaneSurfacePatch(_)));
    shutdown_test_runtimes(&mut server);
}
