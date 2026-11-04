//! Compare decoded masks, allowing rasterizer rounding but no geometry changes.
use std::path::Path;

fn decode(path: &Path) -> (u32, u32, Vec<u8>) {
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut decoder = png::Decoder::new(file);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    bytes.truncate(info.buffer_size());
    (info.width, info.height, bytes)
}

fn main() {
    let dir = std::env::args_os().nth(1).expect("rendered directory");
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/tray");
    let mut count = 0;
    for state in [
        "normal", "checking", "waiting", "conflict", "syncing", "alert", "down",
    ] {
        for (size, suffix) in [(18, ""), (36, "@2x")] {
            let mut masks = Vec::new();
            for kind in ["glyph", "badge"] {
                let name = format!("{kind}-{state}{suffix}.png");
                let (w, h, a) = decode(&committed.join(&name));
                let (rw, rh, b) = decode(&Path::new(&dir).join(&name));
                assert_eq!((w, h), (size, size), "{name}");
                assert_eq!((w, h), (rw, rh), "{name}");
                for (a, b) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
                    assert_eq!(&a[..3], &[0, 0, 0], "{name}: colored mask");
                    assert_eq!(&b[..3], &[0, 0, 0], "{name}: colored render");
                    assert!(a[3].abs_diff(b[3]) <= 2, "{name}: alpha changed");
                }
                masks.push(a);
                count += 1;
            }
            assert!(
                masks[0]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(masks[1].as_chunks::<4>().0.iter())
                    .all(|(a, b)| a[3] <= 2 || b[3] <= 2),
                "{state}: glyph overlaps badge"
            );
        }
    }
    println!("{count} masks reproduced; dimensions, black alpha and knockout separation passed");
}
