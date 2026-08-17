#!/usr/bin/env python3
"""
Vocab binary builder for yb-reader.
Compiles 45,000+ English words with frequency-based difficulty (0-100 / CEFR A1-C2)
and concise WordNet glosses into a high-speed binary database: vocab.bin.
"""

import os
import sys
import struct
import urllib.request
import zipfile
import io
import re

MAGIC = b"YBVOC01\0"

def clean_word(w):
    return re.sub(r'[^a-zA-Z\-]', '', w).lower().strip()

def clean_gloss(raw_def):
    if not raw_def:
        return ""
    # Strip example sentences after semicolon or quote
    first_clause = re.split(r'[;"]', raw_def)[0].strip()
    # Strip leading parenthesis qualifiers like "(of language)"
    first_clause = re.sub(r'^\([^\)]+\)\s*', '', first_clause)
    first_clause = first_clause.strip()
    if len(first_clause) > 55:
        first_clause = first_clause[:52] + "..."
    return first_clause.capitalize()

def parse_wordnet_file(z, filename):
    entries = {}
    content = z.read(filename).decode("utf-8", errors="ignore")
    for line in content.splitlines():
        if line.startswith("  ") or not line.strip():
            continue
        parts = line.split(" | ")
        if len(parts) >= 2:
            gloss = clean_gloss(parts[1].strip())
            meta = parts[0].split()
            if len(meta) >= 5:
                try:
                    w_count = int(meta[3], 16)
                    for i in range(w_count):
                        word = meta[4 + i * 2].lower().replace("_", "-")
                        w_clean = clean_word(word)
                        if w_clean and w_clean not in entries:
                            entries[w_clean] = gloss
                except:
                    pass
    return entries

def build():
    print("Building vocabulary database...")

    # 1. Download WordNet
    tmp_wn = "/tmp/wordnet.zip"
    if not os.path.exists(tmp_wn):
        print("Downloading WordNet database...")
        urllib.request.urlretrieve("https://raw.githubusercontent.com/nltk/nltk_data/gh-pages/packages/corpora/wordnet.zip", tmp_wn)

    with open(tmp_wn, "rb") as f:
        z = zipfile.ZipFile(io.BytesIO(f.read()))

    all_defs = {}
    for fname in ["wordnet/data.noun", "wordnet/data.verb", "wordnet/data.adj", "wordnet/data.adv"]:
        d = parse_wordnet_file(z, fname)
        all_defs.update(d)
    print(f"Loaded {len(all_defs)} clean WordNet definitions.")

    # 2. Download Frequency Rankings
    freq_url = "https://raw.githubusercontent.com/IlyaSemenov/wikipedia-word-frequency/master/results/enwiki-2023-04-13.txt"
    tmp_freq = "/tmp/en_freq.txt"
    if not os.path.exists(tmp_freq):
        print("Downloading English word frequency rankings...")
        urllib.request.urlretrieve(freq_url, tmp_freq)

    word_ranks = {}
    with open(tmp_freq, "r", encoding="utf-8", errors="ignore") as f:
        for rank, line in enumerate(f, 1):
            parts = line.strip().split()
            if parts:
                w = clean_word(parts[0])
                if w and w not in word_ranks:
                    word_ranks[w] = rank
            if rank >= 80000:
                break
    print(f"Loaded {len(word_ranks)} frequency ranks.")

    # 3. Process candidate words
    vocab = {}
    
    # First pass: Ranked words with WordNet definitions
    for w, rank in word_ranks.items():
        w_clean = clean_word(w)
        if len(w_clean) < 2 or len(w_clean) > 28:
            continue

        gloss_en = all_defs.get(w_clean, "")
        if not gloss_en and rank > 25000:
            continue

        # Difficulty: 0 (most common) to 100 (rarest/advanced)
        if rank <= 1000:
            cefr = 1  # A1
            diff = int(rank * 15 / 1000)
        elif rank <= 3000:
            cefr = 2  # A2
            diff = 15 + int((rank - 1000) * 15 / 2000)
        elif rank <= 7500:
            cefr = 3  # B1
            diff = 30 + int((rank - 3000) * 20 / 4500)
        elif rank <= 16000:
            cefr = 4  # B2
            diff = 50 + int((rank - 7500) * 20 / 8500)
        elif rank <= 32000:
            cefr = 5  # C1
            diff = 70 + int((rank - 16000) * 18 / 16000)
        else:
            cefr = 6  # C2
            diff = 88 + min(12, int((rank - 32000) * 12 / 20000))

        vocab[w_clean] = (diff, cefr, gloss_en, "")

    # Second pass: ensure rich literary/advanced WordNet words are included
    for w_clean, gloss_en in all_defs.items():
        if w_clean not in vocab and 3 <= len(w_clean) <= 24:
            # Unranked rare words -> C2 difficulty (92)
            vocab[w_clean] = (92, 6, gloss_en, "")

    sorted_words = sorted(vocab.keys())
    count = len(sorted_words)
    print(f"Compiling binary table for {count} words...")

    strings_buf = bytearray()
    index_entries = []

    for w in sorted_words:
        diff, cefr, gloss_en, gloss_tr = vocab[w]

        w_bytes = w.encode("utf-8")
        w_off = len(strings_buf)
        w_len = len(w_bytes)
        strings_buf.extend(w_bytes)

        en_bytes = gloss_en.encode("utf-8")
        en_off = len(strings_buf)
        en_len = len(en_bytes)
        strings_buf.extend(en_bytes)

        tr_bytes = gloss_tr.encode("utf-8")
        tr_off = len(strings_buf)
        tr_len = len(tr_bytes)
        strings_buf.extend(tr_bytes)

        index_entries.append((w_off, w_len, diff, cefr, en_off, en_len, tr_off, tr_len))

    index_table_size = count * 20
    strings_offset = 16 + index_table_size

    out_path = "tools/build_vocab/vocab.bin"
    with open(out_path, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<II", count, strings_offset))
        for entry in index_entries:
            f.write(struct.pack("<IHBB IHIH", 
                entry[0], entry[1], entry[2], entry[3],
                entry[4], entry[5], entry[6], entry[7]
            ))
        f.write(strings_buf)

    size_mb = os.path.getsize(out_path) / (1024 * 1024)
    print(f"Successfully generated {out_path}: {count} words ({size_mb:.2f} MB)")

if __name__ == "__main__":
    build()
