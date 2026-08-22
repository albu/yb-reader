use criterion::{black_box, criterion_group, criterion_main, Criterion};
use yread::fb2::parse_fb2;
use yread::font::FontSystem;
use yread::paginate::{paginate_chapter, LayoutConfig};
use yread::raster::Rasterizer;
use yread::shape::ShapeCache;

const BENCH_FB2: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description><title-info><book-title>Bench</book-title></title-info></description>
<body>
  <section>
    <title><p>Chapter One</p></title>
    <p>Well, Prince, so Genoa and Lucca are now just family estates of the Buonapartes. But I warn you, if you don't tell me that this means war, if you still try to defend the infamies and horrors perpetrated by that Antichrist—I really believe he is Antichrist—I will have nothing more to do with you and you are no longer my friend, no longer my faithful slave, as you call yourself!</p>
    <p>It was in July, 1805, and the speaker was the well-known Anna Pavlovna Scherer, maid of honor and favorite of the Empress Marya Fedorovna. With these words she greeted Prince Vasili Kuragin, a man of high rank and importance, who was the first to arrive at her reception.</p>
    <p>Anna Pavlovna had had a cough for some days. She was, as she said, suffering from la grippe; grippe being then a new word in St. Petersburg, used only by the elite.</p>
    <p>All her invitations without exception, written in French, and delivered by a scarlet-liveried footman that morning, ran as follows: If you have nothing better to do, Count (or Prince), and if the prospect of spending an evening with a poor invalid is not too terrible, I shall be very charmed to see you tonight between 7 and 10.</p>
    <p>Heavens! what a virulent attack! replied the prince, not in the least recalcitrant at this reception. He had just entered, wearing an embroidered court uniform, knee breeches, and shoes, and had stars on his coat and a serene expression on his flat face.</p>
  </section>
</body>
</FictionBook>"##;

fn bench_pagination_and_rendering(c: &mut Criterion) {
    let book = parse_fb2(BENCH_FB2.as_bytes()).expect("parse");
    let chapter = &book.chapters[0];
    let fonts = FontSystem::default();
    let config = LayoutConfig::default();

    c.bench_function("paginate_chapter_warm_cache", |b| {
        let mut cache = ShapeCache::new();
        // Warm up cache
        let _ = paginate_chapter(chapter, &config, &fonts, &mut cache, Some(hypher::Lang::English));
        b.iter(|| {
            let (pt, layouts) = paginate_chapter(
                black_box(chapter),
                black_box(&config),
                black_box(&fonts),
                black_box(&mut cache),
                black_box(Some(hypher::Lang::English)),
            );
            black_box((pt, layouts));
        });
    });

    c.bench_function("render_page_grayscale", |b| {
        let mut cache = ShapeCache::new();
        let (_pt, layouts) = paginate_chapter(chapter, &config, &fonts, &mut cache, Some(hypher::Lang::English));
        let mut raster = Rasterizer::new();
        let mut fb = vec![255u8; 1236 * 1648];

        b.iter(|| {
            raster.render_page(
                black_box(&book),
                black_box(&layouts[0]),
                black_box(&config),
                black_box(&fonts),
                black_box(&mut fb),
                black_box(1236),
            );
        });
    });
}

criterion_group!(benches, bench_pagination_and_rendering);
criterion_main!(benches);
