pub mod model;
pub mod fb2;
pub mod epub;
pub mod font;
pub mod shape;
pub mod line;
pub mod paginate;
pub mod raster;

pub use model::{Book, Chapter, Block, Run, Style, FontStyle, TextAlign, PageBreak, ChapterPageTable};

/// THE hyphenation-language mapping — the app backend, the background
/// paginator and the bench tool must all hyphenate identically or page
/// tables silently disagree. (This used to exist as two forks that
/// drifted apart on Italian.)
pub fn hypher_lang(language: &str) -> hypher::Lang {
    match language.to_lowercase().as_str() {
        s if s.starts_with("ru") => hypher::Lang::Russian,
        s if s.starts_with("de") => hypher::Lang::German,
        s if s.starts_with("fr") => hypher::Lang::French,
        s if s.starts_with("es") => hypher::Lang::Spanish,
        s if s.starts_with("it") => hypher::Lang::Italian,
        _ => hypher::Lang::English,
    }
}

/// Bench helper kept for examples/bench_chapter.rs — delegates to the
/// canonical mapping.
pub fn paginate_bench_lang(language: &str) -> hypher::Lang {
    hypher_lang(language)
}
