//! Native export integration tests for file delivery, retirement, and fallback.
#![cfg(unix)]

use super::*;
use crate::kitty_graphics::surface::{DeliveryCache, NATIVE_SLOT_BIT, NATIVE_TRANSFER_BIT};
use crate::protocol::{
    SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat, SurfaceGraphicsPlacement,
    SurfaceGraphicsScene, SurfaceGraphicsSource, SurfaceGraphicsTarget,
};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn native_started(server: &mut HeadlessServer, client: u64, transfer: u64, image: u32) -> bool {
    server.native_started(client, transfer, image)
}

fn native_result(
    server: &mut HeadlessServer,
    client: u64,
    transfer: u64,
    image: u32,
    success: bool,
) -> bool {
    server
        .native_result(client, transfer, image, success)
        .unwrap_or(false)
}

fn commit_scene(server: &mut HeadlessServer, client: u64, scene: &SurfaceGraphicsScene) {
    let inline_assets = scene
        .assets
        .iter()
        .map(|asset| asset.key.clone())
        .collect::<Vec<_>>();
    server
        .native_graphics
        .commit_scene(client, scene, &inline_assets);
}

fn scene() -> SurfaceGraphicsScene {
    scene_for(1, 123)
}

fn scene_for(image_id: u32, data_fingerprint: u64) -> SurfaceGraphicsScene {
    let key = SurfaceGraphicsAssetKey {
        source: SurfaceGraphicsSource::Terminal {
            target: SurfaceGraphicsTarget::Pane {
                pane_id: "native-test".into(),
            },
            image_id,
        },
        image_width: 1,
        image_height: 1,
        format: SurfaceGraphicsFormat::Rgba,
        data_len: 4,
        data_fingerprint,
    };
    SurfaceGraphicsScene {
        assets: vec![SurfaceGraphicsAsset {
            key: key.clone(),
            data: vec![1, 2, 3, 255],
        }],
        placements: vec![SurfaceGraphicsPlacement {
            asset: key,
            logical_placement_id: 1,
            x: 2,
            y: 3,
            cols: 1,
            rows: 1,
            source_x: 0,
            source_y: 0,
            source_width: 1,
            source_height: 1,
            x_offset: 0,
            y_offset: 0,
            z: 0,
            scrollback_offset: 0,
        }],
        retained_assets: Vec::new(),
    }
}

fn add_client(server: &mut HeadlessServer, id: u64) -> (Receiver<Vec<u8>>, Receiver<Vec<u8>>) {
    let (writer, control, render) = test_client_writer();
    let mut client = ClientConnection::new(
        (80, 24),
        crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        },
        id,
        RenderEncoding::SemanticFrame,
        Some(writer),
    );
    client.mode = ClientConnectionMode::ClientShell;
    client.direct_graphics = true;
    client.pixel_mouse = true;
    server.clients.insert(id, client);
    server.foreground_client_id = Some(id);
    (control, render)
}

fn prepare_and_commit(server: &mut HeadlessServer, id: u64) -> (PathBuf, u64, u32) {
    let mut desired = scene();
    let (pending, message) = server
        .native_graphics
        .prepare(
            id,
            &server.client_shell_boot_id,
            &mut desired,
            &DeliveryCache::default(),
        )
        .expect("eligible native export");
    let ServerMessage::GraphicsFile {
        path,
        transfer_id,
        image_id,
        expected_len,
        leading,
        control,
        surface_asset,
    } = message
    else {
        panic!("expected file upload");
    };
    assert_ne!(transfer_id & NATIVE_TRANSFER_BIT, 0);
    assert_eq!(expected_len, 4);
    assert!(leading.is_empty());
    assert_eq!(control, format!("a=t,f=32,s=1,v=1,i={image_id},q=0"));
    assert_eq!(surface_asset.as_ref(), Some(&desired.placements[0].asset));
    assert!(desired.assets.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3, 255]);
    commit_scene(server, id, &desired);
    server.native_graphics.commit(id, pending);
    (PathBuf::from(path), transfer_id, image_id)
}

fn prepare_scene_and_commit(
    server: &mut HeadlessServer,
    id: u64,
    mut desired: SurfaceGraphicsScene,
) -> (u64, u32) {
    let (pending, message) = server
        .native_graphics
        .prepare(
            id,
            &server.client_shell_boot_id,
            &mut desired,
            &DeliveryCache::default(),
        )
        .expect("eligible native export");
    let ServerMessage::GraphicsFile {
        transfer_id,
        image_id,
        ..
    } = message
    else {
        panic!("expected file upload");
    };
    commit_scene(server, id, &desired);
    server.native_graphics.commit(id, pending);
    (transfer_id, image_id)
}

fn ack_native(server: &mut HeadlessServer, id: u64, transfer: u64, image: u32) {
    assert!(native_started(server, id, transfer, image));
    native_result(server, id, transfer, image, true);
    assert!(!server.native_graphics.is_pending(id));
}

fn assert_retirement(control: &Receiver<Vec<u8>>, transfer: u64, image: u32) {
    assert!(matches!(
        read_server_message(control.recv_timeout(Duration::from_millis(100)).expect("control retirement")),
        ServerMessage::GraphicsTransmissionRetired { transfer_id, image_id }
            if transfer_id == transfer && image_id == image
    ));
}

fn assert_native_disabled(server: &mut HeadlessServer, id: u64) {
    let mut desired = scene();
    assert!(server
        .native_graphics
        .prepare(
            id,
            &server.client_shell_boot_id,
            &mut desired,
            &DeliveryCache::default(),
        )
        .is_none());
    assert_eq!(desired.assets[0].data, [1, 2, 3, 255]);
    assert!(server.clients[&id].direct_graphics);
}

#[test]
fn native_tokens_require_started_and_old_api_tokens_are_ignored() {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let (path, token, image) = prepare_and_commit(&mut server, 7);

    assert!(!native_result(&mut server, 7, token, image, true));
    assert!(server.native_graphics.is_pending(7));
    assert!(path.exists());
    assert!(!native_started(
        &mut server,
        7,
        token & !NATIVE_TRANSFER_BIT,
        image
    ));
    assert!(!native_result(
        &mut server,
        7,
        token & !NATIVE_TRANSFER_BIT,
        image,
        true,
    ));
    assert!(native_started(&mut server, 7, token, image));
    assert!(!native_result(&mut server, 7, token, image, true));
    assert!(!server.native_graphics.is_pending(7));
    assert!(!path.exists());
}

#[test]
fn native_image_banks_advance_only_on_started_ack_and_are_scoped_and_pruned() {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let (_other_control, _other_render) = add_client(&mut server, 8);
    let base = crate::kitty_graphics::surface::native_host_image_id(
        &server.client_shell_boot_id,
        &scene().placements[0].asset,
    );

    // Unknown sources start in bank 1, avoiding an inline image at the base ID.
    let (first_transfer, first_image) = prepare_scene_and_commit(&mut server, 7, scene());
    assert_eq!(first_image, base ^ NATIVE_SLOT_BIT);
    // A success before Started is not an ACK and neither completes nor advances.
    assert!(!native_result(
        &mut server,
        7,
        first_transfer,
        first_image,
        true
    ));
    assert!(server.native_graphics.is_pending(7));
    ack_native(&mut server, 7, first_transfer, first_image);

    let (second_transfer, second_image) =
        prepare_scene_and_commit(&mut server, 7, scene_for(1, 124));
    assert_eq!(second_image, base);
    // A late ACK for the preceding transfer cannot advance the pending revision.
    assert!(!native_result(
        &mut server,
        7,
        first_transfer,
        first_image,
        true
    ));
    assert!(server.native_graphics.is_pending(7));
    ack_native(&mut server, 7, second_transfer, second_image);
    let (third_transfer, third_image) = prepare_scene_and_commit(&mut server, 7, scene_for(1, 125));
    assert_eq!(third_image, base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 7, third_transfer, third_image);

    // Selection is independent per client.
    let (other_transfer, other_image) = prepare_scene_and_commit(&mut server, 8, scene_for(1, 126));
    assert_eq!(other_image, base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 8, other_transfer, other_image);

    // A different logical source also starts in bank 1. Retention keeps source
    // 1's bank history even though it has no placement in this scene.
    let mut source_two = scene_for(2, 127);
    source_two
        .retained_assets
        .push(scene_for(1, 127).placements[0].asset.clone());
    let source_two_base = crate::kitty_graphics::surface::native_host_image_id(
        &server.client_shell_boot_id,
        &source_two.placements[0].asset,
    );
    let (source_two_transfer, source_two_image) =
        prepare_scene_and_commit(&mut server, 7, source_two);
    assert_eq!(source_two_image, source_two_base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 7, source_two_transfer, source_two_image);
    let (retained_transfer, retained_image) =
        prepare_scene_and_commit(&mut server, 7, scene_for(1, 128));
    assert_eq!(retained_image, base);
    ack_native(&mut server, 7, retained_transfer, retained_image);

    // Move source 1 back to bank 1, then omit it entirely. Returning after an
    // unrelated upload starts at bank 1 rather than retaining stale history.
    let (before_prune_transfer, before_prune_image) =
        prepare_scene_and_commit(&mut server, 7, scene_for(1, 129));
    assert_eq!(before_prune_image, base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 7, before_prune_transfer, before_prune_image);
    let (pruning_transfer, pruning_image) =
        prepare_scene_and_commit(&mut server, 7, scene_for(2, 130));
    assert_eq!(pruning_image, source_two_base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 7, pruning_transfer, pruning_image);
    let (restored_transfer, restored_image) =
        prepare_scene_and_commit(&mut server, 7, scene_for(1, 131));
    assert_eq!(restored_image, base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 7, restored_transfer, restored_image);

    // Reusing a disconnected client ID must not inherit its source-bank history.
    server.remove_client(7);
    let (_control, _render) = add_client(&mut server, 7);
    let (_reconnected_transfer, reconnected_image) =
        prepare_scene_and_commit(&mut server, 7, scene_for(1, 132));
    assert_eq!(reconnected_image, base ^ NATIVE_SLOT_BIT);
}

#[test]
fn inline_commit_updates_bank_only_for_a_new_asset_revision() {
    // Replaying inline bytes for the exact ACKed key still addresses the native
    // bank retained by the client, so it must not be recorded as bank 0.
    let mut exact = test_headless_server();
    let (_control, _render) = add_client(&mut exact, 7);
    let base = crate::kitty_graphics::surface::native_host_image_id(
        &exact.client_shell_boot_id,
        &scene().placements[0].asset,
    );
    let (first_transfer, first_image) = prepare_scene_and_commit(&mut exact, 7, scene());
    assert_eq!(first_image, base ^ NATIVE_SLOT_BIT);
    ack_native(&mut exact, 7, first_transfer, first_image);
    commit_scene(&mut exact, 7, &scene());
    let (_next_transfer, next_image) = prepare_scene_and_commit(&mut exact, 7, scene_for(1, 124));
    assert_eq!(next_image, base);

    // A new inline revision has no native key mapping on the client and is
    // uploaded at the logical base ID. The next native revision must therefore
    // stage into bank 1 rather than collide with that visible base image.
    let mut revised = test_headless_server();
    let (_control, _render) = add_client(&mut revised, 7);
    let (first_transfer, first_image) = prepare_scene_and_commit(&mut revised, 7, scene());
    ack_native(&mut revised, 7, first_transfer, first_image);
    commit_scene(&mut revised, 7, &scene_for(1, 124));
    let (_next_transfer, next_image) = prepare_scene_and_commit(&mut revised, 7, scene_for(1, 125));
    assert_eq!(next_image, base ^ NATIVE_SLOT_BIT);
}

#[test]
fn rejected_omitted_scene_does_not_prune_visible_bank_history() {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let base = crate::kitty_graphics::surface::native_host_image_id(
        &server.client_shell_boot_id,
        &scene().placements[0].asset,
    );
    let (first_transfer, first_image) = prepare_scene_and_commit(&mut server, 7, scene());
    assert_eq!(first_image, base ^ NATIVE_SLOT_BIT);
    ack_native(&mut server, 7, first_transfer, first_image);

    // Preparing an omitted-source scene is speculative. Model the full render
    // path rejecting it because its render lane is already occupied.
    let writer = ClientWriter::test_paused();
    server.clients.get_mut(&7).unwrap().writer = Some(writer.clone());
    writer.render.try_send(vec![1]).unwrap();
    let mut omitted = SurfaceGraphicsScene::default();
    assert!(server
        .prepare_native_scene(
            7,
            &mut omitted,
            &mut DeliveryCache::default(),
            &mut Default::default(),
        )
        .is_none());
    assert!(matches!(
        writer.render.try_send(vec![2]),
        Err(std::sync::mpsc::TrySendError::Full(_))
    ));

    // The rejected omission did not change what is visible. Stage opposite the
    // still-visible bank 1 when the source returns.
    let (_next_transfer, next_image) = prepare_scene_and_commit(&mut server, 7, scene_for(1, 124));
    assert_eq!(next_image, base);
}

#[test]
fn unqueued_native_prepare_does_not_advance_image_bank() {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let base = crate::kitty_graphics::surface::native_host_image_id(
        &server.client_shell_boot_id,
        &scene_for(3, 200).placements[0].asset,
    );
    let mut dropped_scene = scene_for(3, 200);
    let (dropped, dropped_message) = server
        .native_graphics
        .prepare(
            7,
            &server.client_shell_boot_id,
            &mut dropped_scene,
            &DeliveryCache::default(),
        )
        .unwrap();
    let ServerMessage::GraphicsFile {
        transfer_id: dropped_transfer,
        image_id: dropped_image,
        ..
    } = dropped_message
    else {
        panic!("expected file upload");
    };
    assert_eq!(dropped_image, base ^ NATIVE_SLOT_BIT);
    drop(dropped); // Model serialization/send failure before commit.

    let (transfer, image) = prepare_scene_and_commit(&mut server, 7, scene_for(3, 201));
    assert_eq!(image, dropped_image);
    assert!(!native_result(
        &mut server,
        7,
        dropped_transfer,
        dropped_image,
        true
    ));
    assert!(server.native_graphics.is_pending(7));
    ack_native(&mut server, 7, transfer, image);
    let (_next_transfer, next_image) = prepare_scene_and_commit(&mut server, 7, scene_for(3, 202));
    assert_eq!(next_image, base);
}

#[test]
fn native_timeout_uses_control_lane_and_falls_back_inline() {
    let mut server = test_headless_server();
    let (control, render) = add_client(&mut server, 7);
    let (path, token, image) = prepare_and_commit(&mut server, 7);
    // Occupy the render lane: retirement must not wait for file/render backpressure.
    server.clients[&7]
        .writer
        .as_ref()
        .unwrap()
        .render
        .send_ordered(vec![99])
        .unwrap();
    assert!(server.expire_native_graphics(Instant::now() + Duration::from_secs(60)));
    assert_retirement(&control, token, image);
    assert_eq!(
        render.recv_timeout(Duration::from_millis(100)).unwrap(),
        vec![99]
    );
    assert!(render.try_recv().is_err());
    assert!(!path.exists());
    assert!(!server.native_graphics.is_pending(7));
    assert!(!server.clients[&7].shell_graphics_delivery.has_pending());
    assert_eq!(server.clients[&7].deferred_render(), DeferredRender::Full);
    assert_native_disabled(&mut server, 7);
    assert!(!native_result(&mut server, 7, token, image, true));
}

#[test]
fn native_rejection_falls_back_without_disabling_other_client() {
    let mut server = test_headless_server();
    let (control, render) = add_client(&mut server, 7);
    let (_other_control, _other_render) = add_client(&mut server, 8);
    let (path, token, image) = prepare_and_commit(&mut server, 7);
    let (other_path, other_token, other_image) = prepare_and_commit(&mut server, 8);
    assert!(native_result(&mut server, 7, token, image, false));
    assert_retirement(&control, token, image);
    assert!(render.try_recv().is_err());
    assert!(!path.exists());
    assert_native_disabled(&mut server, 7);
    assert!(other_path.exists());
    assert!(server.native_graphics.is_pending(8));
    native_started(&mut server, 8, other_token, other_image);
    native_result(&mut server, 8, other_token, other_image, true);
    assert!(!other_path.exists());
    assert!(!server.native_graphics.is_pending(8));
    assert!(server.clients[&8].direct_graphics);
}

#[test]
fn disconnect_unlinks_only_that_clients_native_export() {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let (_other_control, _other_render) = add_client(&mut server, 8);
    let (path, token, image) = prepare_and_commit(&mut server, 7);
    let (other_path, other_token, other_image) = prepare_and_commit(&mut server, 8);
    server.remove_client(7);
    assert!(!path.exists());
    assert!(other_path.exists());
    assert!(!server.native_graphics.is_pending(7));
    assert!(server.native_graphics.is_pending(8));
    assert!(!native_result(&mut server, 7, token, image, true));
    native_started(&mut server, 8, other_token, other_image);
    native_result(&mut server, 8, other_token, other_image, true);
    assert!(!other_path.exists());
    assert!(!server.native_graphics.is_pending(8));
    assert!(server.clients[&8].direct_graphics);
}

#[test]
fn pending_hold_accepts_pixel_revision_but_not_geometry_or_source_change() {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let (_path, _token, _image) = prepare_and_commit(&mut server, 7);
    let original = scene();
    assert!(server.native_graphics.can_hold(7, &original));
    let mut next = original.clone();
    next.placements[0].asset.data_fingerprint += 1;
    next.assets[0].key.data_fingerprint += 1;
    next.assets[0].data[0] += 1;
    assert!(server.native_graphics.can_hold(7, &next));
    assert_eq!(
        server.native_graphics.hold(7).unwrap().0.placements,
        original.placements
    );
    next.placements[0].cols += 1;
    assert!(!server.native_graphics.can_hold(7, &next));
    next = original.clone();
    next.placements[0].x += 1;
    assert!(!server.native_graphics.can_hold(7, &next));
    next = original.clone();
    next.placements[0].asset.image_width += 1;
    assert!(!server.native_graphics.can_hold(7, &next));
    next = original.clone();
    next.placements[0].asset.source = SurfaceGraphicsSource::Terminal {
        target: SurfaceGraphicsTarget::Pane {
            pane_id: "another-pane".into(),
        },
        image_id: 1,
    };
    assert!(!server.native_graphics.can_hold(7, &next));
    assert!(!server
        .native_graphics
        .can_hold(7, &SurfaceGraphicsScene::default()));
    next = original;
    next.retained_assets.push(next.placements[0].asset.clone());
    assert!(!server.native_graphics.can_hold(7, &next));
}

fn drain_native_render_messages(writer: &ClientWriter) -> Vec<ServerMessage> {
    let mut messages = Vec::new();
    for batch in writer.test_drain() {
        let len = batch.len() as u64;
        let mut cursor = std::io::Cursor::new(batch);
        while cursor.position() < len {
            messages.push(
                protocol::read_message(&mut cursor, protocol::MAX_GRAPHICS_FRAME_SIZE)
                    .expect("decode ordered scene/file transaction"),
            );
        }
    }
    messages
}

fn write_chunked_test_image(
    server: &mut HeadlessServer,
    pane: crate::layout::PaneId,
    image_id: u32,
    side: u32,
    pixels: &[u8],
) {
    use base64::Engine as _;

    let encoded = base64::engine::general_purpose::STANDARD.encode(pixels);
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = u8::from(index + 1 < count);
        let mut command = if index == 0 {
            format!(
                "\x1b_Ga=T,f=32,t=d,i={image_id},p={image_id},s={side},v={side},c=1,r=1,q=2,m={more};"
            )
            .into_bytes()
        } else {
            format!("\x1b_Gm={more};").into_bytes()
        };
        command.extend_from_slice(chunk);
        command.extend_from_slice(b"\x1b\\");
        write_shared_test_pane(server, pane, &command);
    }
}

#[tokio::test]
async fn delta_encoded_inline_commit_updates_next_native_bank() {
    let (mut server, _control, _render, pane) = retained_test_server_with_control(
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
    );
    let writer = ClientWriter::test_paused();
    let client = server.clients.get_mut(&1).unwrap();
    client.writer = Some(writer.clone());
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.render_state.enable_surface_delta(true);
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    client.direct_graphics = true;
    client.pixel_mouse = true;
    server.app.state.kitty_graphics_enabled = true;

    server.render_and_stream();
    let first = drain_native_render_messages(&writer);
    let (first_transfer, first_image) = first
        .iter()
        .find_map(|message| match message {
            ServerMessage::GraphicsFile {
                transfer_id,
                image_id,
                ..
            } => Some((*transfer_id, *image_id)),
            _ => None,
        })
        .expect("first native upload");
    assert!(native_started(&mut server, 1, first_transfer, first_image));
    native_result(&mut server, 1, first_transfer, first_image, true);

    // Model a client without direct-file capability for this replacement.
    // Delta encoding
    // wraps its surface in EndpointControl, but the queued asset still commits
    // bank 0 bookkeeping.
    server.clients.get_mut(&1).unwrap().direct_graphics = false;
    write_shared_test_pane(
        &mut server,
        pane,
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;AP8A/w==\x1b\\",
    );
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
    let inline = drain_native_render_messages(&writer);
    assert!(inline.iter().any(|message| matches!(
        message,
        ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND
    )));
    assert!(!inline
        .iter()
        .any(|message| matches!(message, ServerMessage::GraphicsFile { .. })));

    server.clients.get_mut(&1).unwrap().direct_graphics = true;
    write_shared_test_pane(
        &mut server,
        pane,
        b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;AAD//w==\x1b\\",
    );
    server.render_and_stream();
    let next = drain_native_render_messages(&writer);
    assert!(next.iter().any(|message| matches!(
        message,
        ServerMessage::EndpointControl { kind, .. }
            if kind == protocol::surface_delta::MESSAGE_KIND
    )));
    let next_image = next
        .iter()
        .find_map(|message| match message {
            ServerMessage::GraphicsFile { image_id, .. } => Some(*image_id),
            _ => None,
        })
        .expect("native upload after inline delta");
    assert_eq!(next_image, first_image, "must stage opposite inline bank 0");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn oversized_native_scene_queues_file_and_continues_stripped_assets_after_ack() {
    let (mut server, _control, _render, pane) = retained_test_server_with_control(b"");
    let writer = ClientWriter::test_paused();
    let client = server.clients.get_mut(&1).unwrap();
    client.writer = Some(writer.clone());
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    client.direct_graphics = true;
    client.pixel_mouse = true;
    server.app.state.kitty_graphics_enabled = true;

    const SIDE: u32 = 128;
    let pixels = vec![0x7f; SIDE as usize * SIDE as usize * 4];
    for image_id in [7, 8, 9] {
        write_chunked_test_image(&mut server, pane, image_id, SIDE, &pixels);
    }

    // The limit is deliberately above one 64 KiB asset plus metadata but below
    // the two inline assets left after selecting one native upload.
    server.render_and_stream_with_test_graphics_limit(100_000);
    let first = drain_native_render_messages(&writer);
    let first_surface = first
        .iter()
        .find_map(|message| match message {
            ServerMessage::PaneSurface(surface) => Some(surface),
            _ => None,
        })
        .expect("trimmed metadata scene");
    assert_eq!(first_surface.graphics.placements.len(), 3);
    assert_eq!(first_surface.graphics.assets.len(), 1);
    let first_inline = first_surface.graphics.assets[0].key.clone();
    let (transfer, image, first_native) = first
        .iter()
        .find_map(|message| match message {
            ServerMessage::GraphicsFile {
                transfer_id,
                image_id,
                surface_asset: Some(asset),
                ..
            } => Some((*transfer_id, *image_id, asset.clone())),
            _ => None,
        })
        .expect("selected native asset survives oversized recovery");
    assert_ne!(first_inline.source, first_native.source);
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);

    assert!(native_started(&mut server, 1, transfer, image));
    native_result(&mut server, 1, transfer, image, true);
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);

    server.render_and_stream_with_test_graphics_limit(100_000);
    let continuation = drain_native_render_messages(&writer);
    let continued_asset = continuation.iter().find_map(|message| match message {
        ServerMessage::GraphicsFile {
            surface_asset: Some(asset),
            ..
        } => Some(asset),
        ServerMessage::PaneSurface(surface) => {
            surface.graphics.assets.first().map(|asset| &asset.key)
        }
        _ => None,
    });
    let continued_asset = continued_asset.expect("stripped asset delivered on continuation");
    assert_ne!(continued_asset.source, first_inline.source);
    assert_ne!(continued_asset.source, first_native.source);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn oversized_inline_trims_largest_and_sends_fitting_asset_without_retry_spin() {
    let (mut server, _control, _render, pane) = retained_test_server_with_control(b"");
    let writer = ClientWriter::test_paused();
    let client = server.clients.get_mut(&1).unwrap();
    client.writer = Some(writer.clone());
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    client.direct_graphics = false;
    client.pixel_mouse = true;
    server.app.state.kitty_graphics_enabled = true;

    for (image_id, side) in [(7, 128u32), (8, 16u32)] {
        let pixels = vec![image_id as u8; side as usize * side as usize * 4];
        write_chunked_test_image(&mut server, pane, image_id, side, &pixels);
    }

    // Probe the exact payload size of this surface with only the small asset.
    // Resetting the server-side delivery/baseline makes the measured render
    // observational only; the next call exercises the production recovery path.
    server.render_and_stream();
    let mut measured_surface = drain_native_render_messages(&writer)
        .into_iter()
        .find_map(|message| match message {
            ServerMessage::PaneSurface(surface) => Some(surface),
            _ => None,
        })
        .expect("measurement surface");
    assert_eq!(measured_surface.graphics.assets.len(), 2);
    measured_surface
        .graphics
        .assets
        .retain(|asset| asset.key.image_width == 16);
    let exact_small_limit = HeadlessServer::frame_server_message_with_max(
        &ServerMessage::PaneSurface(measured_surface),
        crate::protocol::MAX_GRAPHICS_FRAME_SIZE,
    )
    .unwrap()
    .len()
    .saturating_sub(4);
    let client = server.clients.get_mut(&1).unwrap();
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    client.shell_graphics_delivery = DeliveryCache::default();
    client.clear_deferred_render();

    // Exactly metadata plus the 1 KiB image fits; adding the 64 KiB image does not.
    server.render_and_stream_with_test_graphics_limit(exact_small_limit);
    let first = drain_native_render_messages(&writer);
    let first_surface = first
        .iter()
        .find_map(|message| match message {
            ServerMessage::PaneSurface(surface) => Some(surface),
            _ => None,
        })
        .expect("surface with fitting inline asset");
    assert_eq!(first_surface.graphics.assets.len(), 1);
    assert_eq!(first_surface.graphics.assets[0].key.image_width, 16);
    assert!(!first
        .iter()
        .any(|message| matches!(message, ServerMessage::GraphicsFile { .. })));
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);

    // The remaining large asset cannot fit even alone. It remains absent from
    // delivery state, but the identical impossible frame is not rescheduled forever.
    server.render_and_stream_with_test_graphics_limit(exact_small_limit);
    let second = drain_native_render_messages(&writer);
    let second_surface = second
        .iter()
        .find_map(|message| match message {
            ServerMessage::PaneSurface(surface) => Some(surface),
            _ => None,
        })
        .expect("metadata-only bounded fallback");
    assert!(second_surface.graphics.assets.is_empty());
    assert!(server.clients[&1].shell_graphics_delivery.has_pending());
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn quiet_native_producer_geometry_retirement_schedules_full_inline_recovery() {
    for retained in [false, true] {
        let (mut server, _control, _render, pane) = retained_test_server_with_control(
            b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\",
        );
        let writer = ClientWriter::test_paused();
        let client = server.clients.get_mut(&1).unwrap();
        client.writer = Some(writer.clone());
        client.mode = ClientConnectionMode::ClientShell;
        client.render_state =
            crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
        client.cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        client.direct_graphics = true;
        client.pixel_mouse = true;
        server.app.state.kitty_graphics_enabled = true;
        server.app.render_dirty.take();
        server.render_and_stream();

        // The writer has already drained, so no future writer progress can wake
        // the server after geometry cancellation. The native ACK is still pending.
        let initial = drain_native_render_messages(&writer);
        let metadata_index = initial
            .iter()
            .position(|message| matches!(message, ServerMessage::PaneSurface(_)))
            .expect("native metadata scene");
        let file_index = initial
            .iter()
            .position(|message| matches!(message, ServerMessage::GraphicsFile { .. }))
            .expect("native file upload");
        assert!(metadata_index < file_index);
        let ServerMessage::PaneSurface(metadata) = &initial[metadata_index] else {
            unreachable!()
        };
        assert!(metadata.graphics.assets.is_empty());
        assert_eq!(metadata.graphics.placements.len(), 1);
        assert_eq!(metadata.graphics.placements[0].cols, 1);
        let ServerMessage::GraphicsFile {
            transfer_id,
            image_id,
            path,
            ..
        } = &initial[file_index]
        else {
            unreachable!()
        };
        let (token, image, native_path) = (*transfer_id, *image_id, PathBuf::from(path));
        assert!(native_path.exists());
        assert!(server.native_graphics.is_pending(1));
        assert!(writer.test_drain().is_empty());
        native_started(&mut server, 1, token, image);

        // A single producer update moves/resizes the placement, then goes quiet.
        // Re-upload the same pixels to replace placement p=3 with two columns.
        write_shared_test_pane(
            &mut server,
            pane,
            b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=2,r=1,q=2;/wAA/w==\x1b\\",
        );
        server.app.render_dirty.take(); // model the main loop consuming this PTY wake
        assert!(!server.app.render_dirty.is_pending());
        if retained {
            assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
        } else {
            server.render_and_stream();
        }
        assert!(!server.native_graphics.is_pending(1), "retained={retained}");
        assert!(!native_path.exists());
        assert!(
            server.app.render_dirty.is_pending(),
            "geometry retirement must independently wake the quiet server; retained={retained}"
        );
        assert!(server.app.render_dirty.has_immediate_work());
        assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);
        let retired = drain_native_render_messages(&writer);
        assert!(retired.iter().any(|message| matches!(message,
            ServerMessage::GraphicsTransmissionRetired { transfer_id, image_id }
                if *transfer_id == token && *image_id == image
        )));
        assert!(!retired.iter().any(|message| matches!(
            message,
            ServerMessage::PaneSurface(_) | ServerMessage::GraphicsFile { .. }
        )));

        // A stale success must not restore native residency or consume the full
        // recovery wake. No producer bytes or writer events occur after this ACK.
        assert!(!native_result(&mut server, 1, token, image, true));
        assert!(!server.native_graphics.is_pending(1));
        let request = server.app.render_dirty.take();
        assert!(
            request.generic,
            "generic full-render request must survive late ACK; retained={retained}"
        );
        server.render_and_stream();
        let recovered = drain_native_render_messages(&writer);
        assert!(!recovered
            .iter()
            .any(|message| matches!(message, ServerMessage::GraphicsFile { .. })));
        let surface = recovered
            .iter()
            .find_map(|message| match message {
                ServerMessage::PaneSurface(surface) => Some(surface),
                _ => None,
            })
            .expect(
                "next scheduled full render delivers inline fallback without new producer activity",
            );
        assert_eq!(surface.graphics.placements.len(), 1);
        assert_eq!(surface.graphics.placements[0].cols, 2);
        assert_eq!(surface.graphics.assets.len(), 1);
        assert_eq!(surface.graphics.assets[0].data, [255, 0, 0, 255]);
        assert!(server.clients[&1].direct_graphics);
        assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
#[ignore = "manual native-file retained-render scaling profile; one image, 1/15 populated panes"]
async fn native_file_render_scale_profile() {
    use base64::Engine as _;
    use ratatui::layout::Direction;

    const WARMUP: usize = 5;
    const SAMPLES: usize = 35;
    const IMAGE_WIDTH: u32 = 800;
    const IMAGE_HEIGHT: u32 = 480;
    for count in [1, 15] {
        for (native, source_retention) in [(false, false), (true, false), (true, true)] {
            let (mut server, _control, _render, root) =
                retained_test_server_with_control(b"populated root terminal\r\n");
            if source_retention {
                // Test runtimes normally leave file transmission disabled. Enable
                // it and the snapshot callback only for this source-producing pane.
                server
                    .app
                    .state
                    .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, root)
                    .unwrap()
                    .test_enable_kitty_source_forwarding();
            }
            let mut panes = vec![root];
            for index in 1..count {
                let workspace = &mut server.app.state.workspaces[0];
                workspace.tabs[0].layout.focus_pane(panes[(index - 1) / 2]);
                let pane = workspace.test_split(if index % 2 == 0 {
                    Direction::Vertical
                } else {
                    Direction::Horizontal
                });
                workspace.insert_test_runtime(
                    pane,
                    crate::terminal::TerminalRuntime::test_with_screen_bytes(
                        160,
                        48,
                        b"populated terminal\r\nsecond row\r\nthird row\r\n",
                    ),
                );
                panes.push(pane);
            }
            let writer = ClientWriter::test_paused();
            let client = server.clients.get_mut(&1).unwrap();
            client.writer = Some(writer.clone());
            client.mode = ClientConnectionMode::ClientShell;
            client.terminal_size = (160, 48);
            client.render_state =
                crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
            client.cell_size = crate::kitty_graphics::HostCellSize {
                width_px: 10,
                height_px: 20,
            };
            // Compare actual capability fallback with automatic native delivery.
            client.direct_graphics = native;
            client.pixel_mouse = true;
            server.app.state.kitty_graphics_enabled = true;
            server.render_and_stream();
            let baseline = drain_native_render_messages(&writer);
            assert!(baseline
                .iter()
                .any(|message| matches!(message, ServerMessage::PaneSurface(_))));
            assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
            let sources = panes.iter().copied().collect::<HashSet<_>>();

            // Deterministic noisy RGB, opaque alpha. Only the first pixel changes
            // each submission; no timing includes producer/base64 construction.
            let mut rgba = vec![0u8; IMAGE_WIDTH as usize * IMAGE_HEIGHT as usize * 4];
            let mut noise = 0x1234_5678u32;
            for pixel in rgba.chunks_exact_mut(4) {
                noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                pixel[..3].copy_from_slice(&noise.to_le_bytes()[..3]);
                pixel[3] = 255;
            }
            // Private /var/tmp native-store generation owns the producer path:
            // bounded to one image and removed even if an assertion unwinds.
            let producer_store = crate::pane_graphics_files::FileStore::native_sources();
            let producer = source_retention
                .then(|| producer_store.export(&rgba).expect("private producer file"));
            let mut previous_source_path: Option<PathBuf> = None;
            let mut times = Vec::with_capacity(SAMPLES);
            for sample in 0..WARMUP + SAMPLES {
                assert!(!server.native_graphics.is_pending(1));
                assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
                assert!(writer.test_drain().is_empty());
                rgba[..3].copy_from_slice(&(sample as u32 + 1).to_le_bytes()[..3]);
                let mut payload = Vec::new();
                payload.extend_from_slice(b"\x1b[H");
                if let Some(producer) = &producer {
                    std::fs::write(producer.path(), &rgba).unwrap();
                    let encoded_path = base64::engine::general_purpose::STANDARD
                        .encode(producer.path().as_os_str().as_encoded_bytes());
                    payload.extend_from_slice(format!(
                        "\x1b_Ga=T,f=32,t=f,i=7,p=3,s={IMAGE_WIDTH},v={IMAGE_HEIGHT},c=1,r=1,q=2;{encoded_path}\x1b\\"
                    ).as_bytes());
                } else {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&rgba);
                    let chunks = encoded.as_bytes().chunks(4096);
                    let chunk_count = chunks.len();
                    for (index, chunk) in chunks.enumerate() {
                        let more = u8::from(index + 1 < chunk_count);
                        if index == 0 {
                            payload.extend_from_slice(format!(
                            "\x1b_Ga=T,f=32,t=d,i=7,p=3,s={IMAGE_WIDTH},v={IMAGE_HEIGHT},c=1,r=1,q=2,m={more};"
                        ).as_bytes());
                        } else {
                            payload.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
                        }
                        payload.extend_from_slice(chunk);
                        payload.extend_from_slice(b"\x1b\\");
                    }
                }
                for pane in &panes {
                    write_shared_test_pane(
                        &mut server,
                        *pane,
                        format!("\x1b[Hframe {sample:03}").as_bytes(),
                    );
                }
                write_shared_test_pane(&mut server, root, &payload);
                let retained_source_path = if source_retention {
                    let runtime = server
                        .app
                        .state
                        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, root)
                        .unwrap();
                    let placements =
                        runtime.kitty_image_placements_with_data_filter(|descriptor| {
                            assert!(descriptor.source_file,
                            "source profile requires working CoW retention, not decoded fallback");
                            false
                        });
                    assert_eq!(placements.len(), 1);
                    assert!(placements[0].data.is_empty());
                    let source = placements[0].source_file.as_ref().expect("retained source");
                    let path = source.path().to_owned();
                    if let Some(previous) = previous_source_path.take() {
                        assert_ne!(previous, path);
                        assert!(!previous.exists(), "replacement releases previous source");
                    }
                    Some(path)
                } else {
                    None
                };
                server.app.render_dirty.take();

                let started = Instant::now();
                let retained = server.render_retained_pane_surface_and_stream(&sources);
                let elapsed = started.elapsed();
                assert!(
                    retained,
                    "profile must not silently fall back to full rendering"
                );
                if sample >= WARMUP {
                    times.push(elapsed);
                }

                // Validate outside timing. Decoded exports close on ACK; source
                // snapshots remain owned by the image until its replacement.
                let messages = drain_native_render_messages(&writer);
                let surfaces = messages
                    .iter()
                    .filter_map(|message| match message {
                        ServerMessage::PaneSurface(surface) => Some(surface),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(surfaces.len(), 1);
                let graphics = &surfaces[0].graphics;
                assert_eq!(graphics.placements.len(), 1);
                assert_eq!(graphics.placements[0].asset.image_width, IMAGE_WIDTH);
                assert_eq!(graphics.placements[0].asset.image_height, IMAGE_HEIGHT);
                let files = messages
                    .iter()
                    .filter_map(|message| match message {
                        ServerMessage::GraphicsFile {
                            path,
                            expected_len,
                            transfer_id,
                            image_id,
                            ..
                        } => Some((path, *expected_len, *transfer_id, *image_id)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if native {
                    assert!(graphics.assets.is_empty());
                    assert_eq!(files.len(), 1);
                    let (path, expected_len, token, image) = files[0];
                    assert_eq!(expected_len, rgba.len() as u64);
                    assert_eq!(std::fs::read(path).unwrap(), rgba);
                    assert_ne!(token & NATIVE_TRANSFER_BIT, 0);
                    native_started(&mut server, 1, token, image);
                    native_result(&mut server, 1, token, image, true);
                    if source_retention {
                        assert_eq!(
                            retained_source_path.as_deref(),
                            Some(std::path::Path::new(path))
                        );
                        assert!(
                            std::path::Path::new(path).exists(),
                            "ACK must preserve image backing"
                        );
                        previous_source_path = Some(PathBuf::from(path));
                    } else {
                        assert!(!std::path::Path::new(path).exists());
                    }
                } else {
                    assert!(files.is_empty());
                    assert_eq!(graphics.assets.len(), 1);
                    assert_eq!(graphics.assets[0].data, rgba);
                }
                assert!(!server.native_graphics.is_pending(1));
                assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
            }
            times.sort_unstable();
            println!(
                "native_file_render_scale retained=true native_files={native} source_retention={source_retention} viewport=160x48 populated_panes={count} changing_images=1 image=800x480 warmup={WARMUP} samples={SAMPLES} median_us={} p95_us={}",
                times[SAMPLES / 2].as_micros(), times[SAMPLES * 95 / 100].as_micros(),
            );
            shutdown_test_runtimes(&mut server);
            drop(server);
            if let Some(path) = previous_source_path {
                // Aborted compression tasks release their terminal reference on poll.
                for _ in 0..100 {
                    if !path.exists() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(!path.exists(), "terminal shutdown releases final source");
            }
        }
    }
}

fn source_backed_scene(
    source: &Arc<crate::pane_graphics_files::OwnedExport>,
) -> (
    SurfaceGraphicsScene,
    crate::kitty_graphics::surface::SourceFiles,
) {
    let mut desired = scene();
    let key = &mut desired.assets[0].key;
    key.image_width = (source.len() / 4) as u32;
    key.data_len = source.len() as u64;
    key.data_fingerprint = source.fingerprint();
    desired.placements[0].asset = key.clone();
    desired.placements[0].source_width = key.image_width;
    let sources = std::collections::HashMap::from([(key.clone(), Arc::clone(source))]);
    desired.assets[0].data.clear();
    (desired, sources)
}

fn assert_source_upload_and_ack(
    source: &Arc<crate::pane_graphics_files::OwnedExport>,
    pixels: &[u8],
) {
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    let (mut desired, mut sources) = source_backed_scene(source);
    let mut delivery = DeliveryCache::default();
    let (pending, message) = server
        .prepare_native_scene(7, &mut desired, &mut delivery, &mut sources)
        .expect("source-backed native upload");
    assert!(desired.assets.is_empty(), "metadata must not carry pixels");
    assert_eq!(desired.placements.len(), 1);
    assert!(sources.is_empty());
    let ServerMessage::GraphicsFile {
        path,
        transfer_id,
        image_id,
        expected_len,
        surface_asset,
        leading,
        control,
    } = message
    else {
        panic!("expected path-only upload");
    };
    assert_eq!(
        PathBuf::from(&path),
        source.path(),
        "reuse source, not decoded export"
    );
    assert_eq!(expected_len, pixels.len() as u64);
    assert_eq!(surface_asset.as_ref(), Some(&desired.placements[0].asset));
    assert!(leading.is_empty());
    assert_eq!(
        control,
        format!("a=t,f=32,s={},v=1,i={image_id},q=0", pixels.len() / 4)
    );
    assert_ne!(transfer_id & NATIVE_TRANSFER_BIT, 0);
    assert_eq!(std::fs::read(&path).unwrap(), pixels);
    commit_scene(&mut server, 7, &desired);
    server.native_graphics.commit(7, pending);
    native_started(&mut server, 7, transfer_id, image_id);
    native_result(&mut server, 7, transfer_id, image_id, true);
    assert!(!server.native_graphics.is_pending(7));
    // The producer image's Arc still owns the backing after the transfer ACK.
    assert_eq!(std::fs::read(&path).unwrap(), pixels);
    assert_eq!(source.copy_rgba().unwrap(), pixels);
}

#[test]
fn source_backed_native_upload_reuses_path_and_ack_preserves_image_backing() {
    let store = crate::pane_graphics_files::FileStore::native_sources();
    let pixels = [1, 2, 3, 255];
    let source = Arc::new(store.export(&pixels).unwrap());
    let path = source.path().to_owned();
    assert_source_upload_and_ack(&source, &pixels);
    drop(source);
    assert!(!path.exists(), "final image owner releases the backing");
}

#[test]
fn source_backed_client_without_direct_support_materializes_exact_pixels() {
    let store = crate::pane_graphics_files::FileStore::native_sources();
    let pixels = [1, 2, 3, 255, 200, 100, 50, 0];
    let source = Arc::new(store.export(&pixels).unwrap());
    let (mut desired, mut sources) = source_backed_scene(&source);
    let expected_placements = desired.placements.clone();
    let mut server = test_headless_server();
    let (_control, _render) = add_client(&mut server, 7);
    server.clients.get_mut(&7).unwrap().direct_graphics = false;
    assert!(server
        .prepare_native_scene(7, &mut desired, &mut DeliveryCache::default(), &mut sources)
        .is_none());
    assert!(sources.is_empty());
    assert_eq!(desired.assets.len(), 1);
    assert_eq!(desired.assets[0].data, pixels);
    assert_eq!(desired.placements, expected_placements);
    assert!(!server.native_graphics.is_pending(7));
}

#[cfg(target_os = "linux")]
#[test]
fn snapshotted_source_survives_producer_unlink_and_native_ack() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt as _;

    let store = crate::pane_graphics_files::FileStore::native_sources();
    let producer_path = store.source_directory().unwrap().join("producer.rgba");
    let pixels = vec![73; 4096];
    let mut producer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&producer_path)
        .unwrap();
    std::io::Write::write_all(&mut producer, &pixels).unwrap();
    let snapshot = store.snapshot(i64::from(producer.as_raw_fd()), pixels.len());
    std::fs::remove_file(&producer_path).unwrap();
    drop(producer);
    let source = match snapshot {
        Ok(source) => Arc::new(source),
        Err(error) => {
            assert!(
                error.kind() == std::io::ErrorKind::Unsupported
                    || matches!(
                        error.raw_os_error(),
                        Some(
                            libc::EOPNOTSUPP
                                | libc::ENOTTY
                                | libc::EXDEV
                                | libc::EINVAL
                                | libc::ENOSYS
                        )
                    ),
                "unexpected snapshot failure: {error}"
            );
            eprintln!("native snapshot unsupported on test filesystem: {error}");
            return;
        }
    };
    assert!(!producer_path.exists());
    let path = source.path().to_owned();
    assert_source_upload_and_ack(&source, &pixels);
    drop(source);
    assert!(!path.exists());
}

#[tokio::test]
async fn failed_source_read_retries_after_identical_suppression_without_producer_activity() {
    let (mut server, _control, _render, _pane) =
        retained_test_server_with_control(b"quiet source producer");
    let writer = ClientWriter::test_paused();
    let client = server.clients.get_mut(&1).unwrap();
    client.writer = Some(writer.clone());
    client.mode = ClientConnectionMode::ClientShell;
    client.render_state =
        crate::server::render_stream::ClientRenderState::new(RenderEncoding::SemanticFrame);
    server.clients.get_mut(&1).unwrap().direct_graphics = false;
    server.render_and_stream();
    let mut surface = drain_native_render_messages(&writer)
        .into_iter()
        .find_map(|message| match message {
            ServerMessage::PaneSurface(surface) => Some(surface),
            _ => None,
        })
        .expect("initial quiet surface");

    // Inject the collected sidecar at the scheduler boundary, avoiding global
    // environment changes and a filesystem-dependent CoW requirement.
    let store = crate::pane_graphics_files::FileStore::native_sources();
    let pixels = [1, 2, 3, 255];
    let source = Arc::new(store.export(&pixels).unwrap());
    std::fs::write(source.path(), []).unwrap();
    let mut delivery = DeliveryCache::default();
    for attempt in 0..2 {
        let (mut graphics, mut sources) = source_backed_scene(&source);
        assert!(server
            .prepare_native_scene(1, &mut graphics, &mut delivery, &mut sources)
            .is_none());
        assert!(graphics.assets.is_empty());
        surface.graphics = graphics;
        let client = server.clients.get_mut(&1).unwrap();
        let prepared = client.render_state.prepare_pane_surface(surface.clone());
        if attempt == 0 {
            client
                .render_state
                .commit_sent_frame(prepared.expect("first failed scene"));
        } else {
            assert!(
                prepared.is_none(),
                "repeated read failure produces identical metadata"
            );
            // Exactly the full-render identical-frame suppression branch.
            client.clear_deferred_render();
        }
    }
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
    server.app.render_dirty.take();

    // The timer is one-shot: an inactive client does not receive repeated wakes
    // unless a real materialization attempt fails and explicitly re-arms it.
    let tick = Instant::now() + Duration::from_secs(2);
    assert!(server.expire_native_graphics(tick));
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);
    server.clients.get_mut(&1).unwrap().clear_deferred_render();
    assert!(!server.expire_native_graphics(tick + Duration::from_secs(3600)));
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);

    let (mut failed_graphics, mut failed_sources) = source_backed_scene(&source);
    assert!(server
        .prepare_native_scene(1, &mut failed_graphics, &mut delivery, &mut failed_sources)
        .is_none());
    assert!(failed_graphics.assets.is_empty());
    let rearmed_tick = tick + Duration::from_secs(7200);
    assert!(server.expire_native_graphics(rearmed_tick));
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::Full);

    // No PTY bytes, writer progress, or native ACK occurs after recovery.
    std::fs::write(source.path(), pixels).unwrap();
    let (mut graphics, mut sources) = source_backed_scene(&source);
    assert!(server
        .prepare_native_scene(1, &mut graphics, &mut delivery, &mut sources)
        .is_none());
    assert_eq!(graphics.assets[0].data, pixels);
    surface.graphics = graphics;
    let client = server.clients.get_mut(&1).unwrap();
    let prepared = client
        .render_state
        .prepare_pane_surface(surface)
        .expect("recovered pixels must not be suppressed as identical");
    let ServerMessage::PaneSurface(recovered) = prepared.message() else {
        panic!("expected recovered surface");
    };
    assert_eq!(recovered.graphics.assets[0].data, pixels);
    client.render_state.commit_sent_frame(prepared);
    client.clear_deferred_render();
    assert!(
        !server.expire_native_graphics(rearmed_tick + Duration::from_secs(2)),
        "successful materialization leaves the one-shot retry timer disarmed"
    );
    assert_eq!(server.clients[&1].deferred_render(), DeferredRender::None);
    shutdown_test_runtimes(&mut server);
}
