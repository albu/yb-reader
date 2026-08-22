pub mod model;
pub mod fb2;
pub mod epub;
pub mod font;
pub mod shape;
pub mod line;
pub mod paginate;
pub mod raster;

pub use model::{Book, Chapter, Block, Run, Style, FontStyle, TextAlign, PageBreak, ChapterPageTable};

/// Bench helper: hyphenation language from metadata (mirrors the app's
/// hypher_lang_for — the two must agree or page tables differ).
pub fn paginate_bench_lang(language: &str) -> hypher::Lang {
    match language.to_lowercase().as_str() {
        s if s.starts_with("ru") => hypher::Lang::Russian,
        s if s.starts_with("de") => hypher::Lang::German,
        s if s.starts_with("fr") => hypher::Lang::French,
        s if s.starts_with("es") => hypher::Lang::Spanish,
        _ => hypher::Lang::English,
    }
}
