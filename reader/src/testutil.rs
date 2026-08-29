//! Test-only helpers shared by the screen tests. Declared with
//! `#[cfg(test)]` in main.rs, so release builds never see this file.

/// Write the 1236x1648 grayscale canvas as a PNG preview artifact when
/// YB_AI_PREVIEW_DIR (or ARTIFACT_DIR) is set; a silent no-op otherwise.
/// One copy here replaces the five hand-duplicated versions that had
/// started drifting.
pub(crate) fn save_preview_artifact(name: &str, canvas: &[u8]) {
    let artifact_dir = match std::env::var("YB_AI_PREVIEW_DIR").or_else(|_| std::env::var("ARTIFACT_DIR"))
    {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };
    let path = std::path::Path::new(&artifact_dir).join(name);
    if let Ok(file) = std::fs::File::create(&path) {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut w) = enc.write_header() {
            let _ = w.write_image_data(canvas);
        }
    }
}
