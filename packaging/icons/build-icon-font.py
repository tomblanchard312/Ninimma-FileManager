#!/usr/bin/env python3
# Ninimma modification: independent project packaging and branding.
"""Builds assets/fonts/icons.ttf: the Lucide icons the interface uses, each
at the code point of the glyph the source code writes ("📂" draws Lucide's
folder-open). The UI's strings keep their emoji; the app puts this font
first, so they render as Lucide icons in the theme's text colour, and the
emoji font stays as a fallback for anything not mapped.

Needs fontTools (pip install fonttools) and Lucide's font files:

    v=1.59.0
    curl -LO https://unpkg.com/lucide-static@$v/font/lucide.ttf
    curl -LO https://unpkg.com/lucide-static@$v/font/info.json
    python3 packaging/icons/build-icon-font.py lucide.ttf info.json

Lucide is ISC-licensed (packaging/icons/LICENSE-lucide).
"""

import json
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable

HERE = Path(__file__).resolve().parent
OUT = HERE.parent.parent / "assets" / "fonts" / "icons.ttf"
FAMILY = "Ninimma Icons"
# egui takes a line's height and baseline from the first font in a family,
# and the icon font goes first (so it wins over text fonts that also have
# arrows or stars): it gets the vertical metrics of egui's text font,
# Ubuntu Light (1000 units per em, like Lucide), so lines don't change.
TEXT_METRICS = {"ascent": 932, "descent": -189, "line_gap": 28,
                "typo": (776, -185, 56), "win": (932, 189)}


def read_map():
    pairs = []
    for line in (HERE / "map.txt").read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        glyph, name = line.split(" ", 1)
        if len(glyph) != 1:
            sys.exit(f"one character per line, not {glyph!r}")
        pairs.append((ord(glyph), name.strip()))
    return pairs


def main(font_path, info_path):
    info = json.loads(Path(info_path).read_text(encoding="utf-8"))
    pairs = read_map()
    font = TTFont(font_path)
    lucide_cmap = font.getBestCmap()
    wanted = {}
    for codepoint, name in pairs:
        if name not in info:
            sys.exit(f"Lucide has no icon named {name!r}")
        lucide_cp = int(info[name]["encodedCode"].lstrip("\\"), 16)
        wanted[codepoint] = lucide_cmap[lucide_cp]

    # Keep only the glyphs used (by name), then point the UI's code points at them.
    options = subset.Options()
    options.glyph_names = True
    options.notdef_outline = True
    options.name_IDs = ["*"]
    options.layout_features = []
    sub = subset.Subsetter(options)
    sub.populate(glyphs=sorted(set(wanted.values())))
    sub.subset(font)

    cmap = newTable("cmap")
    cmap.tableVersion = 0
    bmp = CmapSubtable.newSubtable(4)
    bmp.platformID, bmp.platEncID, bmp.language = 3, 1, 0
    bmp.cmap = {cp: g for cp, g in wanted.items() if cp <= 0xFFFF}
    full = CmapSubtable.newSubtable(12)
    full.platformID, full.platEncID, full.language = 3, 10, 0
    full.cmap = dict(wanted)
    cmap.tables = [bmp, full]
    font["cmap"] = cmap

    hhea, os2 = font["hhea"], font["OS/2"]
    assert font["head"].unitsPerEm == 1000
    hhea.ascent, hhea.descent, hhea.lineGap = (
        TEXT_METRICS["ascent"], TEXT_METRICS["descent"], TEXT_METRICS["line_gap"])
    os2.sTypoAscender, os2.sTypoDescender, os2.sTypoLineGap = TEXT_METRICS["typo"]
    os2.usWinAscent, os2.usWinDescent = TEXT_METRICS["win"]

    for record in font["name"].names:
        if record.nameID in (1, 4, 16):
            record.string = FAMILY
        elif record.nameID == 6:
            record.string = FAMILY.replace(" ", "")
    OUT.parent.mkdir(parents=True, exist_ok=True)
    font.save(OUT)
    print(f"{OUT}: {len(wanted)} code points, {len(set(wanted.values()))} icons, {OUT.stat().st_size} bytes")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
