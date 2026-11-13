# /// script
# requires-python = ">=3.15"
# dependencies = ["fonttools==4.68.0"]
# ///

import argparse
import hashlib
from pathlib import Path
from typing import Final

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

_SOURCE_SHA256: Final = (
    "bfb7bb691513f12e734dc346c03a03f784912432d7e3fa8e56efcf906fe86b3d"
)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if hashlib.sha256(args.source.read_bytes()).hexdigest() != _SOURCE_SHA256:
        parser.error("source differs from the pinned Noto Sans font")
    font = TTFont(args.source, recalcTimestamp=False)
    instantiateVariableFont(font, {"wght": 400, "wdth": 100}, inplace=True)
    options = subset.Options()
    options.recalc_timestamp = False
    sub = subset.Subsetter(options=options)
    sub.populate(unicodes=range(0x0300, 0x0400))
    sub.populate(unicodes=range(0x1F00, 0x2000))
    sub.subset(font)
    names = font["name"]
    for record in list(names.names):
        if record.nameID in (1, 4, 6, 16):
            names.setName(
                "CodestrandGreek" if record.nameID == 6 else "Codestrand Greek",
                record.nameID,
                record.platformID,
                record.platEncID,
                record.langID,
            )
    font.save(args.output)
    print(f"{args.output.stat().st_size} bytes: {args.output}")


if __name__ == "__main__":
    main()
