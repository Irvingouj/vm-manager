#!/usr/bin/env python3
"""WCAG 2.x contrast ratios for colour pairs.

    contrast.py '#1c1917' '#f6f2ea' [more pairs: fg bg fg bg ...]
    contrast.py --tokens light.txt     # lines: name fg bg [min ratio]; // comments

Colours are #rgb or #rrggbb. Prints each pair's ratio and whether it meets
4.5:1 (text), 3:1 (large text, non-text) and 7:1; with --tokens, exits 1 when a
pair is under its own minimum (default 4.5). No dependency.
"""
import sys


def channel(v: int) -> float:
    c = v / 255
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def luminance(colour: str) -> float:
    h = colour.strip().lstrip("#")
    if len(h) == 3:
        h = "".join(ch * 2 for ch in h)
    if len(h) != 6:
        raise SystemExit(f"not a #rgb or #rrggbb colour: {colour}")
    r, g, b = (int(h[i : i + 2], 16) for i in (0, 2, 4))
    return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)


def ratio(fg: str, bg: str) -> float:
    a, b = sorted((luminance(fg), luminance(bg)), reverse=True)
    return (a + 0.05) / (b + 0.05)


def verdict(r: float) -> str:
    return "AAA text" if r >= 7 else "AA text" if r >= 4.5 else "large text / non-text only" if r >= 3 else "FAILS"


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return 2
    failed = 0
    if args[0] == "--tokens":
        for line in open(args[1], encoding="utf-8"):
            parts = line.split()
            # Blank lines and lines beginning with // are skipped.
            if not parts or parts[0].startswith("//"):
                continue
            if len(parts) < 3:
                raise SystemExit(f"expected `name fg bg [min ratio]`, got: {line.strip()}")
            name, fg, bg = parts[0], parts[1], parts[2]
            least = float(parts[3]) if len(parts) > 3 else 4.5
            r = ratio(fg, bg)
            ok = r >= least
            failed += not ok
            print(f"{'ok  ' if ok else 'FAIL'} {name:28} {fg} on {bg}  {r:5.2f}:1 (needs {least}:1)")
        return 1 if failed else 0
    if len(args) % 2:
        raise SystemExit("give colours in pairs: fg bg")
    for fg, bg in zip(args[::2], args[1::2]):
        r = ratio(fg, bg)
        print(f"{fg} on {bg}  {r:5.2f}:1  {verdict(r)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
