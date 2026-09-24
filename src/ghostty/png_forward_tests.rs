use super::*;
use base64::Engine as _;

fn fixture() -> Vec<u8> {
    fixture_size(32, 16)
}

fn fixture_size(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&vec![42; (width * height * 4) as usize])
            .unwrap();
    }
    bytes
}

fn terminal() -> Terminal {
    let mut terminal = Terminal::new(20, 10, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.set_kitty_png_forwarding(true).unwrap();
    terminal.resize(20, 10, 8, 16).unwrap();
    terminal
}

#[test]
#[ignore = "manual fixed-geometry native PNG ingestion/extraction scaling profile"]
fn native_png_render_scale_profile() {
    let png = fixture_size(800, 480);
    let upload = format!(
        "\x1b[H\x1b_Ga=T,f=100,i=7,c=60,r=20,C=1,q=2;{}\x1b\\",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    for count in [1, 15] {
        for forwarding in [false, true] {
            let mut panes = (0..count)
                .map(|_| {
                    let mut pane = Terminal::new(120, 40, 0).unwrap();
                    pane.enable_kitty_graphics().unwrap();
                    pane.set_kitty_png_forwarding(forwarding).unwrap();
                    pane.resize(120, 40, 8, 16).unwrap();
                    pane
                })
                .collect::<Vec<_>>();
            let mut samples = Vec::new();
            for iteration in 0..65 {
                let begin = std::time::Instant::now();
                for pane in &mut panes {
                    pane.write(upload.as_bytes());
                    let placements = pane.kitty_image_placements().unwrap();
                    assert_eq!(placements.len(), 1);
                    assert_eq!(
                        placements[0].data.len(),
                        if forwarding { png.len() } else { 800 * 480 * 4 }
                    );
                    std::hint::black_box(placements);
                }
                if iteration >= 5 {
                    samples.push(begin.elapsed().as_micros());
                }
            }
            samples.sort_unstable();
            eprintln!("native PNG panes={count} forwarding={forwarding} median_us={} p95_us={} png_bytes={}", samples[30], samples[57], png.len());
        }
    }
}

#[test]
fn normal_kitty_graphics_fully_decodes_quiet_png_uploads() {
    let mut terminal = Terminal::new(20, 10, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(20, 10, 8, 16).unwrap();
    let before = PNG_DECODE_CALLS.get();
    terminal.write(
        format!(
            "\x1b_Ga=T,f=100,i=7,c=4,r=1,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(fixture())
        )
        .as_bytes(),
    );
    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].format, KittyImageFormat::Rgba);
    assert_eq!(placements[0].data, vec![42; 32 * 16 * 4]);
    assert_eq!(PNG_DECODE_CALLS.get(), before + 1);
}

#[test]
fn quiet_png_forwarding_retains_exact_payload_without_pixel_decode() {
    let bytes = fixture();
    let mut terminal = terminal();
    let before = PNG_DECODE_CALLS.get();
    terminal.write(
        format!(
            "\x1b_Ga=T,f=100,i=7,c=4,r=1,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        )
        .as_bytes(),
    );
    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].format, KittyImageFormat::Png);
    assert_eq!(placements[0].data, bytes);
    assert_eq!(
        (placements[0].image_width, placements[0].image_height),
        (32, 16)
    );
    assert_eq!(PNG_DECODE_CALLS.get(), before);
}

#[test]
fn placeholder_png_forwarding_and_lazy_animation_decode() {
    let bytes = fixture();
    let mut terminal = terminal();
    let before = PNG_DECODE_CALLS.get();
    terminal.write(
        format!(
            "\x1b_Ga=T,f=100,U=1,i=7,c=4,r=1,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        )
        .as_bytes(),
    );
    terminal.write("\x1b[H\x1b[38;2;0;0;7m\u{10eeee}\u{0305}\u{0305}\x1b[0m".as_bytes());
    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].format, KittyImageFormat::Png);
    assert_eq!(placements[0].data, bytes);
    assert_eq!(PNG_DECODE_CALLS.get(), before);
    let fingerprint = placements[0].data_fingerprint;
    terminal.write(b"\x1b_Ga=a,i=7,q=2;\x1b\\");
    let materialized = terminal.kitty_image_placements().unwrap();
    assert_eq!(PNG_DECODE_CALLS.get(), before + 1);
    assert_eq!(materialized[0].format, KittyImageFormat::Rgba);
    assert_eq!(materialized[0].data, [42; 32 * 16 * 4]);
    assert_ne!(materialized[0].data_fingerprint, fingerprint);
}

#[test]
fn experimental_quiet_png_defers_crc_valid_compressed_data_errors() {
    let mut bytes = fixture();
    let mut offset = 8;
    loop {
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        if &bytes[offset + 4..offset + 8] == b"IDAT" {
            bytes[offset + 8] = 0; // Invalid zlib header, but structurally valid PNG chunks.
            let mut crc = !0u32;
            for &byte in &bytes[offset + 4..offset + 8 + len] {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
                }
            }
            bytes[offset + 8 + len..offset + 12 + len].copy_from_slice(&(!crc).to_be_bytes());
            break;
        }
        offset += len + 12;
    }
    for quiet in [0, 2] {
        let mut terminal = terminal();
        let before = PNG_DECODE_CALLS.get();
        terminal.write(
            format!(
                "\x1b_Ga=T,f=100,i=7,c=4,r=1,q={quiet};{}\x1b\\",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            )
            .as_bytes(),
        );
        let placements = terminal.kitty_image_placements().unwrap();
        if quiet == 0 {
            assert!(placements.is_empty());
            assert_eq!(PNG_DECODE_CALLS.get(), before + 1);
        } else {
            assert_eq!(placements[0].data, bytes);
            assert_eq!(PNG_DECODE_CALLS.get(), before);
            terminal.write(b"\x1b_Ga=a,i=7,q=2;\x1b\\");
            assert_eq!(PNG_DECODE_CALLS.get(), before + 1);
            assert_eq!(terminal.kitty_image_placements().unwrap()[0].data, bytes);
        }
    }
}

#[test]
fn response_bearing_png_and_queries_keep_full_validation() {
    let bytes = fixture();
    for (action, quiet) in [('T', 0), ('T', 1), ('q', 2)] {
        let mut terminal = terminal();
        let before = PNG_DECODE_CALLS.get();
        terminal.write(
            format!(
                "\x1b_Ga={action},f=100,i=7,c=4,r=1,q={quiet};{}\x1b\\",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            )
            .as_bytes(),
        );
        assert_eq!(
            PNG_DECODE_CALLS.get(),
            before + 1,
            "action {action}, quiet {quiet}"
        );
        let placements = terminal.kitty_image_placements().unwrap();
        if action == 'q' {
            assert!(placements.is_empty());
        } else {
            assert_eq!(placements[0].format, KittyImageFormat::Rgba);
        }
    }
}

#[test]
fn chunked_png_forwarding_inherits_quiet_mode() {
    let bytes = fixture();
    let mut terminal = terminal();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let split = (encoded.len() / 8) * 4;
    let before = PNG_DECODE_CALLS.get();
    terminal.write(
        format!(
            "\x1b_Ga=T,f=100,i=7,c=4,r=1,q=2,m=1;{}\x1b\\",
            &encoded[..split]
        )
        .as_bytes(),
    );
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
    terminal.write(format!("\x1b_Gm=0;{}\x1b\\", &encoded[split..]).as_bytes());
    assert_eq!(terminal.kitty_image_placements().unwrap()[0].data, bytes);
    assert_eq!(PNG_DECODE_CALLS.get(), before);
}
