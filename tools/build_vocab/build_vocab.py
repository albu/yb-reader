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
    # Strip example sentences starting with quotes (WordNet uses `"example"`)
    parts = re.split(r'\s*;\s*"', raw_def)
    definition = parts[0].strip()
    # Strip leading parenthesis qualifiers like "(of language)"
    definition = re.sub(r'^\([^\)]+\)\s*', '', definition).strip()
    return definition.capitalize()


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

    PREFIXES = ["counter", "under", "over", "anti", "semi", "auto", "post", "pre", "non", "mis", "dis", "sub", "super", "inter", "un", "re", "in", "im", "de", "co"]
    SUFFIXES = ["lessness", "fulness", "ability", "ibility", "lessly", "fully", "ation", "ition", "liness", "ness", "able", "ible", "ment", "tion", "sion", "ally", "ized", "ised", "ize", "ise", "est", "ish", "ful", "less", "ing", "ity", "ous", "ive", "ed", "ly", "er", "or", "al", "en", "s"]


    def find_deep_root(w, depth=0):
        if depth > 3:
            return w, word_ranks.get(w, 999999)
        best_root = w
        best_rank = word_ranks.get(w, 999999)

        candidates = [w]
        for p in PREFIXES:
            if w.startswith(p) and len(w) > len(p) + 2:
                candidates.append(w[len(p):])

        for cand in candidates:
            r = word_ranks.get(cand, 999999)
            if r < best_rank:
                best_rank = r
                best_root = cand

            for s in SUFFIXES:
                if cand.endswith(s) and len(cand) > len(s) + 2:
                    stem = cand[:-len(s)]
                    for variant in [stem, stem + "e", stem + "y"]:
                        r_var = word_ranks.get(variant, 999999)
                        if r_var < best_rank:
                            best_rank = r_var
                            best_root = variant
                        # Recurse one more level
                        if depth < 2:
                            rec_root, rec_rank = find_deep_root(variant, depth + 1)
                            if rec_rank < best_rank:
                                best_rank = rec_rank
                                best_root = rec_root

        return best_root, best_rank

    def is_transparent(defn):
        d = defn.lower().strip()
        if (d.startswith("in a ") or d.startswith("in an ")) and (d.endswith("manner") or d.endswith("way")):
            return True
        if d.startswith("not ") and len(d.split()) <= 4:
            return True
        if d.startswith("the quality of being ") or d.startswith("the state of being ") or d.startswith("quality of being "):
            return True
        if d.startswith("having the quality of ") or d.startswith("an quality of "):
            return True
        return False

    def rank_to_diff(r):
        if r <= 1000:
            return int(r * 15 / 1000), 1
        elif r <= 3000:
            return 15 + int((r - 1000) * 15 / 2000), 2
        elif r <= 7500:
            return 30 + int((r - 3000) * 20 / 4500), 3
        elif r <= 16000:
            return 50 + int((r - 7500) * 20 / 8500), 4
        elif r <= 32000:
            return 70 + int((r - 16000) * 18 / 16000), 5
        else:
            return 88 + min(12, int((r - 32000) * 12 / 20000)), 6

    # 3. Process candidate words
    vocab = {}
    
    # First pass: Ranked words with WordNet definitions
    for w, raw_rank in word_ranks.items():
        w_clean = clean_word(w)
        if len(w_clean) < 2 or len(w_clean) > 28:
            continue

        gloss_en = all_defs.get(w_clean, "")
        if not gloss_en and raw_rank > 25000:
            continue

        # Morphological root decomposition
        root, root_rank = find_deep_root(w_clean)
        
        # Transparent definitions ("In a ... manner", "Not ...") or common roots
        if is_transparent(gloss_en) and root_rank < 30000:
            diff, cefr = 10, 1  # Suppress from automatic annotation
        elif root and root_rank < 12000 and root_rank < raw_rank:
            root_diff, root_cefr = rank_to_diff(root_rank)
            diff = min(55, root_diff + 6)
            cefr = root_cefr if diff < 50 else 3
        else:
            diff, cefr = rank_to_diff(raw_rank)

        vocab[w_clean] = (diff, cefr, gloss_en, "")

    # Second pass: ensure rich literary/advanced WordNet words are included
    for w_clean, gloss_en in all_defs.items():
        if w_clean not in vocab and 3 <= len(w_clean) <= 24:
            root, root_rank = find_deep_root(w_clean)
            if is_transparent(gloss_en) and root_rank < 30000:
                diff, cefr = 10, 1
            elif root and root_rank < 12000:
                root_diff, root_cefr = rank_to_diff(root_rank)
                diff = min(55, root_diff + 6)
                cefr = root_cefr
            else:
                diff = 92
                cefr = 6
            vocab[w_clean] = (diff, cefr, gloss_en, "")



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
