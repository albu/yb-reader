resources/fonts — embedded typefaces (all SIL Open Font License 1.1)
====================================================================
License texts live next to the fonts as OFL-*.txt. Everything is embedded
into the yb-reader binary via include_bytes! in yread/src/font.rs — the app
needs no runtime font files.

Body families (selectable in Quick Settings -> Typography -> FONT):
- Literata (default) — Google Books book serif, organic curves, full
  Latin + Cyrillic. 4 styles, full-size faces.
  (https://github.com/googlefonts/literata)
- PTSerif — ParaType news/book serif with NATIVE Cyrillic (Russian books
  are the primary use case). 4 styles, subsetted.
  (https://github.com/googlefonts/ptserif)
- Bitter — slab serif designed explicitly for screens / e-paper by Sol
  Matas. 4 styles, instanced from the variable font + subsetted.
  (https://github.com/googlefonts/bitter)
- PTSans — ParaType humanist sans with native Cyrillic, the sans option
  (modern / accessible themes). 4 styles, subsetted.
  (https://github.com/googlefonts/ptsans)

Code / fallback / UI:
- NotoSans-Regular — Google Noto Sans, copied from the on-device KOReader
  bundle at /mnt/us/koreader/fonts/noto/NotoSans-Regular.ttf.
  (https://github.com/googlefonts/noto-fonts)

The non-Literata faces are SUBSETTED with pyftsubset to Latin + Cyrillic +
punctuation (~35-190 KB/face vs ~320 KB full) to keep the binary small.
Bitter is instanced from its variable font to static TTFs first — variable
fonts can show rendering quirks on e-ink firmware, and the engine shapes
static faces.
