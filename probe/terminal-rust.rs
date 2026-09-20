#[path = "../src/terminal.rs"]
mod terminal;
#[path = "graphics-corpus.rs"]
mod graphics_corpus;
use terminal::Terminal;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::rc::Rc;

fn main() -> Result<(), String> {
    let mut terminal = Terminal::new(20, 6)?;
    terminal.snapshot()?;
    let bytes = "\x1b[2J\x1b[HASCII e\u{301} 日本語\x1b[2;1H\x1b[1;3;4mStyled\x1b[0m\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x1b\\";
    for byte in bytes.as_bytes() { terminal.feed(&[*byte])?; }
    let snapshot = terminal.snapshot()?;
    let first = snapshot.changed_rows.iter().find(|row| row.index == 0).unwrap();
    assert!(first.cells.iter().any(|cell| cell.text == "e\u{301}"));
    assert!(first.cells.iter().any(|cell| cell.text == "日" && cell.width == 2));
    let second = snapshot.changed_rows.iter().find(|row| row.index == 1).unwrap();
    assert!(second.cells.iter().any(|cell| cell.text == "S" && cell.bold && cell.italic && cell.underline));
    assert!(!second.cells.iter().any(|cell| cell.text.contains("http")));
    assert!(terminal.snapshot()?.changed_rows.is_empty());
    terminal.resize(30, 10)?;
    let resized = terminal.snapshot()?;
    assert_eq!((resized.cols, resized.rows), (30, 10));
    assert!(resized.full);
    terminal.reset()?;
    terminal.set_cell_size(17, 36)?;
    assert!(terminal.take_responses()?.is_empty());
    terminal.feed(b"\x1b[3;5H\x1b[6n")?;
    assert_eq!(terminal.take_responses()?, b"\x1b[3;5R");
    assert!(terminal.take_responses()?.is_empty());
    assert!(terminal.feed(&b"\x1b[5n".repeat(8193)).is_err());
    assert!(terminal.take_responses().is_err());
    terminal.reset()?;
    terminal.feed(b"\x1b[5n")?;
    assert_eq!(terminal.take_responses()?, b"\x1b[0n");
    image_contracts()?;
    graphics_corpus::run()?;
    println!("Rust Ghostty device corpus passed: split UTF-8/CSI, graphemes, wide cells, styles, OSC8, dirty acknowledgement, resize/reset, genuine terminal responses, response overflow and recovery");
    Ok(())
}

fn image_terminal() -> Result<Terminal, String> {
    let mut terminal = Terminal::new(20, 8)?;
    terminal.set_cell_size(10, 10)?;
    terminal.snapshot()?;
    terminal.take_responses()?;
    Ok(terminal)
}

fn kitty(terminal: &mut Terminal, controls: &str, payload: &[u8]) -> Result<(), String> {
    terminal.feed(format!("\x1b_G{controls};{}\x1b\\", STANDARD.encode(payload)).as_bytes())
}

fn images(terminal: &mut Terminal) -> Result<Vec<terminal::ImagePlacement>, String> {
    terminal.snapshot()?.images.ok_or_else(|| "expected a refreshed image snapshot".into())
}

fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

// Tiny deterministic stored-DEFLATE fixtures avoid depending on the decoder's
// compressor implementation. All allocations here are bounded by fixture size.
fn zlib_stored(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len() <= usize::from(u16::MAX));
    let length = bytes.len() as u16;
    let mut encoded = vec![0x78, 0x01, 0x01];
    encoded.extend_from_slice(&length.to_le_bytes());
    encoded.extend_from_slice(&(!length).to_le_bytes());
    encoded.extend_from_slice(bytes);
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in bytes { a = (a + u32::from(byte)) % 65521; b = (b + a) % 65521; }
    encoded.extend_from_slice(&((b << 16) | a).to_be_bytes());
    encoded
}

fn png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = !0u32;
    for &byte in kind.iter().chain(data) {
        crc ^= u32::from(byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1))); }
    }
    png.extend_from_slice(&(!crc).to_be_bytes());
}

fn png_fixture(width: u32, height: u32, depth: u8, color: u8, interlace: bool,
    extra: &[(&[u8; 4], &[u8])], scanlines: &[u8]) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[depth, color, 0, 0, u8::from(interlace)]);
    png_chunk(&mut png, b"IHDR", &header);
    for &(kind, data) in extra { png_chunk(&mut png, kind, data); }
    png_chunk(&mut png, b"IDAT", &zlib_stored(scanlines));
    png_chunk(&mut png, b"IEND", &[]);
    png
}

fn image_contracts() -> Result<(), String> {
    image_decoding()?;
    image_lifecycle()?;
    virtual_images()?;
    image_backgrounds()?;
    println!("Kitty image contracts passed: native query ACK; split PNG palette/gray/16-bit/alpha/Adam7; raw/zlib; generation/cache; delete/reset/screens/scroll/resize; virtual continuation/IDs/fractional crops; no tofu; background layers; invalid/oversized PNG rejection");
    Ok(())
}

fn image_decoding() -> Result<(), String> {
    let mut terminal = image_terminal()?;
    kitty(&mut terminal, "a=q,i=31,f=24,s=1,v=1", &[1, 2, 3])?;
    assert_eq!(terminal.take_responses()?, b"\x1b_Gi=31;OK\x1b\\");
    assert!(images(&mut terminal)?.is_empty(), "query must not store/display an image");

    let fixtures = [
        (png_fixture(2, 1, 8, 6, false, &[], &[0, 10, 20, 30, 0, 40, 50, 60, 128]),
            2, 1, vec![10, 20, 30, 0, 40, 50, 60, 128]),
        (png_fixture(2, 1, 1, 3, false, &[(b"PLTE", &[255, 0, 0, 0, 255, 0]), (b"tRNS", &[255, 64])], &[0, 0x40]),
            2, 1, vec![255, 0, 0, 255, 0, 255, 0, 64]),
        (png_fixture(2, 1, 1, 0, false, &[(b"tRNS", &[0, 1])], &[0, 0x40]),
            2, 1, vec![0, 0, 0, 255, 255, 255, 255, 0]),
        (png_fixture(1, 1, 16, 2, false, &[], &[0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]),
            1, 1, vec![0x12, 0x56, 0x9a, 255]),
        (png_fixture(1, 1, 16, 4, false, &[], &[0, 0xab, 0xcd, 0x12, 0x34]),
            1, 1, vec![0xab, 0xab, 0xab, 0x12]),
        // A 2x2 Adam7 image has data only in passes 1, 6 and 7.
        (png_fixture(2, 2, 8, 6, true, &[], &[0, 255, 0, 0, 255, 0, 0, 255, 0, 128,
            0, 0, 0, 255, 64, 255, 255, 255, 0]),
            2, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 0]),
    ];
    for (png, width, height, rgba) in fixtures {
        terminal.reset()?;
        terminal.snapshot()?;
        let encoded = STANDARD.encode(&png);
        let chunks: Vec<_> = encoded.as_bytes().chunks(12).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            let final_chunk = index + 1 == chunks.len();
            let controls = if index == 0 { "a=T,i=41,p=1,f=100,C=1," } else { "" };
            let wire = format!("\x1b_G{controls}m={};{}\x1b\\", u8::from(!final_chunk),
                std::str::from_utf8(chunk).unwrap());
            for byte in wire.as_bytes() { terminal.feed(&[*byte])?; }
            if !final_chunk {
                assert!(images(&mut terminal)?.is_empty(), "incomplete PNG must not be displayed");
                assert!(terminal.take_responses()?.is_empty(), "incomplete transmission must not ACK");
            }
        }
        let rendered = images(&mut terminal)?;
        assert_eq!(rendered.len(), 1);
        assert_eq!((rendered[0].image.width, rendered[0].image.height), (width, height));
        assert_eq!(rendered[0].image.rgba, rgba);
        assert!(String::from_utf8(terminal.take_responses()?).unwrap().contains(";OK"));
    }

    for compressed in [false, true] {
        terminal.reset()?;
        let rgb = [1, 2, 3, 4, 5, 6];
        let payload = if compressed { zlib_stored(&rgb) } else { rgb.to_vec() };
        let compression = if compressed { ",o=z" } else { "" };
        kitty(&mut terminal, &format!("a=T,i=42,p=1,f=24,s=2,v=1,C=1{compression}"), &payload)?;
        let rendered = images(&mut terminal)?;
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].image.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    let oversized = png_fixture(1 << 24, 2, 8, 6, false, &[], &[]);
    let mut bad_crc = png_fixture(1, 1, 8, 6, false, &[], &[0, 1, 2, 3, 4]);
    bad_crc[29] ^= 1;
    for bad in [b"not PNG".to_vec(), b"\x89PNG\r\n\x1a\n".to_vec(), bad_crc, oversized] {
        terminal.reset()?;
        kitty(&mut terminal, "a=T,i=99,p=1,f=100,C=1", &bad)?;
        assert!(images(&mut terminal)?.is_empty(), "invalid PNG must not be stored/displayed");
        let response = String::from_utf8(terminal.take_responses()?).unwrap();
        assert!(response.contains("i=99") && !response.contains(";OK"), "expected native PNG error: {response:?}");
        terminal.feed(b"\x1b[5n")?;
        assert_eq!(terminal.take_responses()?, b"\x1b[0n", "PNG failure must not corrupt terminal state");
    }
    Ok(())
}

fn image_lifecycle() -> Result<(), String> {
    let mut terminal = image_terminal()?;
    kitty(&mut terminal, "a=T,i=51,p=1,f=32,s=2,v=2,c=2,r=2,C=1,z=-2", &[10; 16])?;
    let first = images(&mut terminal)?.remove(0);
    assert_eq!((first.x, first.y, first.width, first.height, first.z), (0, 0, 20, 20, -2));
    assert_eq!((first.source_x, first.source_y, first.source_width, first.source_height), (0.0, 0.0, 2.0, 2.0));
    assert!(terminal.snapshot()?.images.is_none(), "unchanged snapshot must retain image model");
    terminal.feed(b"\x1b[6;1Hx")?;
    let same = images(&mut terminal)?.remove(0);
    assert!(Rc::ptr_eq(&first.image, &same.image), "text-only dirty frames must reuse owned pixels");
    terminal.feed(b"\x1b[H")?;
    kitty(&mut terminal, "a=T,i=51,p=1,f=32,s=2,v=2,c=2,r=2,C=1,z=-2", &[20; 16])?;
    let replacement = images(&mut terminal)?.remove(0);
    assert_ne!(replacement.image.generation, first.image.generation);
    assert!(!Rc::ptr_eq(&replacement.image, &first.image));
    assert_eq!(replacement.image.rgba, [20; 16]);
    assert_eq!(first.image.rgba, [10; 16], "previous snapshot must remain owned after replacement");

    kitty(&mut terminal, "a=d,d=i,i=51,p=1", &[])?;
    assert!(images(&mut terminal)?.is_empty());
    kitty(&mut terminal, "a=p,i=51,p=2,c=2,r=2,C=1", &[])?;
    let redisplayed = images(&mut terminal)?.remove(0);
    assert_eq!(redisplayed.image.generation, replacement.image.generation, "placement deletion must retain native image data");
    kitty(&mut terminal, "a=d,d=I,i=51", &[])?;
    assert!(images(&mut terminal)?.is_empty());
    terminal.take_responses()?;
    kitty(&mut terminal, "a=p,i=51,p=3,C=1", &[])?;
    assert!(images(&mut terminal)?.is_empty());
    let missing = String::from_utf8(terminal.take_responses()?).unwrap();
    assert!(missing.contains("i=51") && !missing.contains(";OK"));

    terminal.reset()?;
    terminal.feed(b"\x1b[3;1H")?;
    kitty(&mut terminal, "a=T,i=52,p=1,f=24,s=1,v=1,c=2,r=3,C=1", &[1, 2, 3])?;
    let primary = images(&mut terminal)?.remove(0);
    terminal.feed(b"\x1b[?1049h")?;
    assert!(images(&mut terminal)?.is_empty(), "alternate screen must not retain primary images");
    kitty(&mut terminal, "a=T,i=52,p=1,f=24,s=1,v=1,C=1", &[4, 5, 6])?;
    let alternate = images(&mut terminal)?.remove(0);
    assert_ne!(primary.image.generation, alternate.image.generation);
    terminal.feed(b"\x1b[?1049l")?;
    let restored = images(&mut terminal)?.remove(0);
    assert_eq!(restored.image.generation, primary.image.generation);
    assert_eq!(restored.image.rgba, [1, 2, 3, 255]);
    terminal.feed(b"\x1b[8;1H\n")?;
    let scrolled = images(&mut terminal)?.remove(0);
    assert_eq!(scrolled.y, 10, "scrolling must recompute geometry even with unchanged image generation");
    assert_eq!(scrolled.image.generation, primary.image.generation);
    terminal.set_cell_size(12, 15)?;
    let scaled = images(&mut terminal)?.remove(0);
    assert_eq!((scaled.y, scaled.width, scaled.height), (15, 24, 45));
    terminal.resize(22, 9)?;
    let resized = terminal.snapshot()?;
    assert_eq!((resized.cols, resized.rows), (22, 9));
    assert!(resized.images.as_ref().unwrap().iter().all(|p| p.image.generation == primary.image.generation));
    terminal.reset()?;
    assert!(images(&mut terminal)?.is_empty());
    kitty(&mut terminal, "a=T,i=52,p=1,f=24,s=1,v=1,C=1", &[1, 2, 3])?;
    assert_ne!(images(&mut terminal)?[0].image.generation, primary.image.generation, "reset must never alias a prior generation");
    kitty(&mut terminal, "a=p,i=52,p=2,z=5,C=1", &[])?;
    kitty(&mut terminal, "a=p,i=52,p=3,z=-5,C=1", &[])?;
    let ordered = images(&mut terminal)?;
    assert_eq!(ordered.iter().map(|p| p.placement_id).collect::<Vec<_>>(), [3, 1, 2]);
    assert!(ordered.iter().all(|p| Rc::ptr_eq(&p.image, &ordered[0].image)));
    Ok(())
}

fn virtual_images() -> Result<(), String> {
    let mut terminal = image_terminal()?;
    let rgba = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 255];
    kitty(&mut terminal, "a=T,i=42,p=7,f=32,s=2,v=2,U=1,c=4,r=4,C=1", &rgba)?;
    assert!(images(&mut terminal)?.is_empty(), "virtual prototype itself must be invisible");
    terminal.feed("\x1b[38;5;42m\x1b[58;5;7m\u{10eeee}\u{305}\u{305}\u{10eeee}\u{10eeee}\u{10eeee}".as_bytes())?;
    let snapshot = terminal.snapshot()?;
    assert!(snapshot.changed_rows.iter().flat_map(|row| &row.cells).all(|cell| !cell.text.contains('\u{10eeee}')));
    let runs = snapshot.images.unwrap();
    assert_eq!(runs.len(), 1, "omitted diacritics must continue a single row run");
    assert_eq!((runs[0].placement_id, runs[0].width, runs[0].height, runs[0].z), (7, 40, 10, -1));
    near(runs[0].source_width, 2.0);
    near(runs[0].source_height, 0.5);

    // Two separate rows of two placeholders, explicitly restarting each row.
    // Their source areas are sub-texel fragments of the same 2x2 RGBA image.
    terminal.feed("\x1b[2J\x1b[H\u{10eeee}\u{305}\u{305}\u{10eeee}\x1b[2;1H\u{10eeee}\u{30d}\u{305}\u{10eeee}".as_bytes())?;
    let fragments = images(&mut terminal)?;
    assert_eq!(fragments.len(), 2);
    for (index, fragment) in fragments.iter().enumerate() {
        assert_eq!((fragment.x, fragment.y, fragment.width, fragment.height), (0, index as i32 * 10, 20, 10));
        near(fragment.source_x, 0.0);
        near(fragment.source_y, index as f64 * 0.5);
        near(fragment.source_width, 1.0);
        near(fragment.source_height, 0.5);
        assert!(Rc::ptr_eq(&fragment.image, &fragments[0].image));
    }
    // Overwriting placeholders removes their fragments without graphics commands.
    terminal.feed(b"\x1b[H  ")?;
    assert_eq!(images(&mut terminal)?.len(), 1);
    kitty(&mut terminal, "a=d,d=I,i=42", &[])?;
    terminal.set_cell_size(11, 10)?; // Force redraw of the retained placeholder text.
    let deleted = terminal.snapshot()?;
    assert!(deleted.images.unwrap().is_empty());
    assert!(deleted.changed_rows.iter().flat_map(|row| &row.cells).all(|cell| !cell.text.contains('\u{10eeee}')));

    terminal.reset()?;
    kitty(&mut terminal, "a=T,i=33554474,p=7,f=32,s=2,v=2,U=1,c=4,r=4,C=1", &rgba)?;
    kitty(&mut terminal, "a=p,i=33554474,p=8,U=1,c=2,r=2,C=1", &[])?;
    terminal.feed("\x1b[38;2;0;0;42m\x1b[58;2;0;0;7m\u{10eeee}\u{305}\u{305}\u{30e}\u{10eeee}\x1b[2;1H\x1b[58;2;0;0;8m\u{10eeee}\u{305}\u{305}\u{30e}".as_bytes())?;
    let identified = images(&mut terminal)?;
    assert_eq!(identified.len(), 2);
    assert!(identified.iter().all(|p| p.image.id == 33554474));
    let seven = identified.iter().find(|p| p.placement_id == 7).unwrap();
    let eight = identified.iter().find(|p| p.placement_id == 8).unwrap();
    assert_eq!(seven.width, 20, "high image byte must continue into a no-diacritic neighbor");
    near(seven.source_height, 0.5);
    near(eight.source_height, 1.0);
    assert!(Rc::ptr_eq(&seven.image, &eight.image));
    Ok(())
}

fn image_backgrounds() -> Result<(), String> {
    let mut terminal = image_terminal()?;
    terminal.feed(b"a\x1b[48;2;255;255;255mb\x1b[0m\x1b[7mc\x1b[0md")?;
    let snapshot = terminal.snapshot()?;
    let row = snapshot.changed_rows.iter().find(|row| row.index == 0).unwrap();
    for (column, expected) in [(0, true), (1, false), (2, false), (3, true)] {
        let cell = row.cells.iter().find(|cell| cell.column == column).unwrap();
        assert_eq!(cell.background_is_default, expected, "background layer for column {column}");
    }
    assert_eq!(row.cells[0].background, row.cells[1].background,
        "explicit color matching the default still needs a distinct background layer");
    Ok(())
}
