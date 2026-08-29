#!/usr/bin/env python3
"""
Build the builtin English dictionary (wordnet.ybdict) from the raw
Princeton WordNet 3.0 database (the index.* / data.* files).

Usage:
    python3 tools/build_wordnet/build_wordnet.py /path/to/WordNet-3.0/dict \
        resources/wordnet.ybdict

The output is the same YBDICT02 format the reader's StarDict importer
writes: a sorted packed index (normalized word -> display word + meaning)
with meanings read from a blob. Each meaning keeps the word's top senses
(ranked by tagsense count across POS, noun > verb > adj > adv as the
tiebreak) with their quoted example sentences, so the card reads like
WordNet rather than a single terse gloss.

WordNet is licensed separately (see WordNet-3.0/LICENSE) — this tool only
*converts* data the user provides; the .ybdict output is a local artifact
and is never committed to the repo (see .gitignore).
"""

import os
import re
import struct
import sys

MAGIC = b"YBDICT02"
MAX_SENSES = 3


def clean_word(w):
    return "".join(c for c in w.lower() if c.isalpha() or c == "-")


def parse_index(path, max_senses=6):
    """index.<pos> -> {lemma: [(synset offset, tagsense_cnt), ...]}.

    The offsets on a line are listed in frequency order (sense 1 first);
    keep the first `max_senses` of them, all sharing the lemma's
    tagsense_cnt. Ranking across POS and the final per-lemma sense cap
    happen in main().
    """
    out = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith(" "):
                # license preamble + blank lines
                continue
            fields = line.split()
            if len(fields) < 5:
                continue
            lemma = fields[0]
            # fields: lemma pos synset_cnt p_cnt [ptr_symbols...] sense_cnt
            # tagsense_cnt synset_offset...
            try:
                synset_cnt = int(fields[2])
                p_cnt = int(fields[3])
            except ValueError:
                continue
            first_offset = 6 + p_cnt
            if synset_cnt <= 0 or len(fields) < first_offset + synset_cnt:
                continue
            try:
                tagsense = int(fields[first_offset - 1])
            except (IndexError, ValueError):
                tagsense = 0
            senses = out.setdefault(lemma, [])
            for offset in fields[first_offset:first_offset + synset_cnt]:
                if len(senses) < max_senses:
                    senses.append((offset, tagsense))
    return out


def parse_data(path):
    """data.<pos> -> {synset_offset: gloss}."""
    out = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith(" "):
                continue
            offset = line.split(" ", 1)[0]
            if len(offset) != 8 or not offset.isdigit():
                continue
            if "| " not in line:
                continue
            out[offset] = line.split("| ", 1)[1].strip()
    return out


def split_sense(gloss):
    """WordNet gloss -> (definition, [example sentences]).

    A synset gloss is `definition; "example"; "example"`. The definition
    and the examples are shown as separate blocks on the card.
    """
    parts = re.split(r'\s*;\s*"', gloss)
    core = parts[0].strip()
    examples = ['"' + p.rstrip() for p in parts[1:]]
    return core, examples


def format_meaning(senses):
    """senses: [(display, core, examples)] -> display meaning string.

    One sense: just the definition, examples on the next line.
    Several: numbered blocks. Example lines start with a quote so the
    card can indent and dim them.
    """
    lines = []
    for i, (_tagsense, _display, core, examples) in enumerate(senses):
        if len(senses) > 1:
            lines.append(f"{i + 1}. {core}")
        else:
            lines.append(core)
        if examples:
            lines.append(" \u00b7 ".join(examples))
    return "\n".join(lines)


def write_ybdict(path, items):
    """items: list of (clean, display, meaning), sorted + deduped already."""
    count = len(items)
    words = bytearray()
    keys = bytearray()
    data = bytearray()
    index = bytearray()
    for clean, word, meaning in items:
        w_off = len(words)
        words += word.encode("utf-8")
        w_len = len(word.encode("utf-8"))
        k_off = len(keys)
        keys += clean.encode("utf-8")
        k_len = len(clean.encode("utf-8"))
        d_off = len(data)
        data += meaning.encode("utf-8")
        d_len = len(meaning.encode("utf-8"))
        index += struct.pack(">IHIHII", w_off, w_len, k_off, k_len, d_off, d_len)
    words_off = 24 + len(index)
    keys_off = words_off + len(words)
    data_off = keys_off + len(keys)
    with open(path, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack(">IIII", count, words_off, keys_off, data_off))
        f.write(index)
        f.write(words)
        f.write(keys)
        f.write(data)


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 1
    dict_dir, out_path = sys.argv[1], sys.argv[2]

    glosses = {}
    for pos in ("noun", "verb", "adj", "adv"):
        data_path = os.path.join(dict_dir, f"data.{pos}")
        if os.path.exists(data_path):
            glosses.update(parse_data(data_path))

    # Rank senses across POS by tagsense count (noun > verb > adj > adv as
    # the tiebreak) and keep the top few per lemma — with examples. The
    # pre-dictionary tool did the same single-sense ranking; keeping just
    # one terse gloss was why WordNet read as "not verbose".
    pos_priority = {"noun": 4, "verb": 3, "adj": 2, "adv": 1}
    best = {}  # clean -> [(tagsense, display, core, examples), ...]
    for pos in ("noun", "verb", "adj", "adv"):
        idx_path = os.path.join(dict_dir, f"index.{pos}")
        if not os.path.exists(idx_path):
            continue
        prio = pos_priority[pos]
        for lemma, senses in parse_index(idx_path).items():
            clean = clean_word(lemma)
            if not clean:
                continue
            display = lemma.replace("_", " ")
            candidates = []
            for offset, tagsense in senses:
                gloss = glosses.get(offset, "")
                if not gloss:
                    continue
                core, examples = split_sense(gloss)
                if not core:
                    continue
                candidates.append((tagsense, prio, display, core, examples))
            candidates.sort(key=lambda c: (-c[0], -c[1]))
            for tagsense, _prio, disp, core, examples in candidates:
                cur = best.setdefault(clean, [])
                if len(cur) >= MAX_SENSES:
                    break
                if any(c == core for _, _, c, _ in cur):
                    continue
                cur.append((tagsense, disp, core, examples))

    items = []
    for clean, senses in best.items():
        senses.sort(key=lambda s: -s[0])
        display = senses[0][1]
        meaning = format_meaning(senses)
        items.append((clean, display, meaning))
    items.sort(key=lambda t: t[0])
    write_ybdict(out_path, items)
    print(f"wrote {out_path}: {len(items)} lemmas, {os.path.getsize(out_path) / 1e6:.1f} MB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
