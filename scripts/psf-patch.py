#!/usr/bin/env python3
"""Fill in the glyphs a console font is missing for this installer's interface.

Why this exists
---------------
A Linux console font holds EXACTLY 512 glyphs — that is the kernel's ceiling, and
every font on the image is already at it. So a font that is missing three letters
cannot be extended; a glyph has to be given up for each one. That is what this
script does: it works out which glyphs the interface never asks for, and reuses
those slots for the ones it does.

Nothing is invented from nothing. Each missing glyph is built out of shapes the
font already draws, so the result keeps that font's own weight and proportions:

  Є є   the font's own C / c with a central bar
  —     the font's own en dash, widened to the full cell
  Á Ó Ú the acute accent lifted off the font's own É, dropped onto A O U
  Й     the breve lifted off the font's own й, raised to cap height
  ▀ ▄ ▌ pure geometry (top / bottom / left half of the cell)

Licensing decides the NAME, not whether to patch. Terminus is OFL-1.1 with the
Reserved Font Name "Terminus Font", and clause 3 forbids a modified version from
carrying that name — so the patched copies ship as "Artix TUI" / "Artix TUI
Bold", with the licence and provenance beside them. Rename the file before
patching and the new name follows automatically.

Usage (from the repo root)::

    python3 scripts/psf-patch.py --out iso-profile/root-overlay/usr/share/kbd/consolefonts \\
        /tmp/tui-16b.psf.gz

    python3 scripts/psf-patch.py --check <fonts...>   # report coverage only

The set of required characters is read out of `installer/` — the i18n files and
the string literals in the Rust sources — so it cannot drift away from what the
interface actually draws. Output is always PSF2 with a Unicode table, gzipped,
which is what `setfont` wants.
"""

import argparse
import gzip
import os
import re
import struct
import sys

PSF1_MAGIC = b"\x36\x04"
PSF2_MAGIC = b"\x72\xb5\x4a\x86"
PSF1_MODE512 = 0x01
PSF1_MODEHASTAB = 0x02
PSF1_SEPARATOR = 0xFFFE
PSF1_STARTSEQ = 0xFFFF
PSF2_SEPARATOR = 0xFE
PSF2_STARTSEQ = 0xFF

# The kernel loads at most this many glyphs, whatever the file claims.
CONSOLE_GLYPH_LIMIT = 512


class Font:
    """A console font as glyph bitmaps plus, per glyph, the characters it draws.

    `rows[g]` is a list of `height` ints, each a bit-per-pixel row, MSB leftmost.
    `chars[g]` is the set of single characters mapped to glyph `g`; multi-character
    sequences are kept aside in `seqs[g]` so they survive a round trip untouched.
    """

    def __init__(self, width, height, rows, chars, seqs):
        self.width = width
        self.height = height
        self.rows = rows
        self.chars = chars
        self.seqs = seqs

    @property
    def stride(self):
        """Bytes per glyph row."""
        return (self.width + 7) // 8

    @property
    def bits(self):
        """Bits per stored row — a whole number of BYTES, so 9 pixels occupy 16.

        This is the difference that matters when building a glyph: pixel x lives
        at bit `bits - 1 - x`, not `width - 1 - x`. They coincide only when the
        width is a multiple of 8, which is why an 8x16 font looked right while
        9x20 and 12x29 came out as a stripe down the left edge.
        """
        return self.stride * 8

    def bit(self, x):
        return 1 << (self.bits - 1 - x)

    def index_of(self, ch):
        for g, cs in enumerate(self.chars):
            if ch in cs:
                return g
        return None

    def covers(self, ch):
        return self.index_of(ch) is not None


def read_font(path):
    raw = (gzip.open if path.endswith(".gz") else open)(path, "rb").read()
    if raw[:4] == PSF2_MAGIC:
        return _read_psf2(raw)
    if raw[:2] == PSF1_MAGIC:
        return _read_psf1(raw)
    raise ValueError(f"{path}: not a PSF font (magic {raw[:4].hex()})")


def _read_psf1(raw):
    mode, charsize = raw[2], raw[3]
    count = 512 if mode & PSF1_MODE512 else 256
    width, height = 8, charsize
    body = raw[4 : 4 + count * charsize]
    rows = [
        [body[g * charsize + y] for y in range(height)]  # width 8 -> row IS a byte
        for g in range(count)
    ]
    chars = [set() for _ in range(count)]
    seqs = [[] for _ in range(count)]
    if mode & PSF1_MODEHASTAB:
        tab = raw[4 + count * charsize :]
        words = struct.unpack(f"<{len(tab) // 2}H", tab[: len(tab) // 2 * 2])
        g, seq, in_seq = 0, [], False
        for w in words:
            if w == PSF1_STARTSEQ:
                if in_seq and seq:
                    seqs[g].append("".join(seq))
                g, seq, in_seq = g + 1, [], False
                if g >= count:
                    break
            elif w == PSF1_SEPARATOR:
                if in_seq and seq:
                    seqs[g].append("".join(seq))
                seq, in_seq = [], True
            elif in_seq:
                seq.append(chr(w))
            else:
                chars[g].add(chr(w))
    return Font(width, height, rows, chars, seqs)


def _read_psf2(raw):
    _, headersize, flags, count, charsize, height, width = struct.unpack(
        "<7I", raw[4:32]
    )
    stride = (width + 7) // 8
    body = raw[headersize : headersize + count * charsize]
    rows = []
    for g in range(count):
        base = g * charsize
        rows.append(
            [
                int.from_bytes(body[base + y * stride : base + (y + 1) * stride], "big")
                for y in range(height)
            ]
        )
    chars = [set() for _ in range(count)]
    seqs = [[] for _ in range(count)]
    if flags & 1:
        tab = raw[headersize + count * charsize :]
        g, buf, seq_buf, in_seq = 0, bytearray(), [], False
        for byte in tab:
            if byte == PSF2_STARTSEQ:
                _flush(buf, chars, seqs, g, in_seq, seq_buf)
                g, buf, seq_buf, in_seq = g + 1, bytearray(), [], False
                if g >= count:
                    break
            elif byte == PSF2_SEPARATOR:
                _flush(buf, chars, seqs, g, in_seq, seq_buf)
                buf, in_seq = bytearray(), True
            else:
                buf.append(byte)
    return Font(width, height, rows, chars, seqs)


def _flush(buf, chars, seqs, g, in_seq, _seq_buf):
    if not buf:
        return
    text = buf.decode("utf-8", "ignore")
    if in_seq:
        if text:
            seqs[g].append(text)
    else:
        chars[g].update(text)


def write_font(font, path):
    """Always PSF2 with a Unicode table: one format out, whatever came in."""
    stride, charsize = font.stride, font.stride * font.height
    body = bytearray()
    for glyph in font.rows:
        for row in glyph:
            body += row.to_bytes(stride, "big")
    table = bytearray()
    for g in range(len(font.rows)):
        for ch in sorted(font.chars[g]):
            table += ch.encode("utf-8")
        for seq in font.seqs[g]:
            table.append(PSF2_SEPARATOR)
            table += seq.encode("utf-8")
        table.append(PSF2_STARTSEQ)
    head = PSF2_MAGIC + struct.pack(
        "<7I", 0, 32, 1, len(font.rows), charsize, font.height, font.width
    )
    with gzip.open(path, "wb", compresslevel=9) as fh:
        fh.write(bytes(head + body + table))


# ---------------------------------------------------------------- glyph shapes


def ink_rows(font, g):
    """Indices of the rows that have any ink — the glyph's vertical extent."""
    return [y for y, row in enumerate(font.rows[g]) if row]


def ink_cols(font, g):
    cols = []
    for x in range(font.width):
        bit = font.bit(x)
        if any(row & bit for row in font.rows[g]):
            cols.append(x)
    return cols


def full_row(font):
    """Every pixel of the cell — and only those: the padding bits past `width`
    stay clear, so a 12-pixel-wide glyph does not claim 16."""
    mask = 0
    for x in range(font.width):
        mask |= font.bit(x)
    return mask


def widen_dash(font, donor):
    """The font's own dash, stretched across the whole cell — that is an em dash."""
    return [full_row(font) if row else 0 for row in font.rows[donor]]


def bar_through(font, donor):
    """C -> Є, c -> є: the donor plus a central horizontal bar.

    The bar spans from the donor's own left edge to two thirds of its width, at
    the vertical middle of its ink, which is where Є carries it.
    """
    ys, xs = ink_rows(font, donor), ink_cols(font, donor)
    if not ys or not xs:
        return None
    mid = ys[len(ys) // 2]
    # A tall font deserves a thicker bar, or it disappears next to the stroke.
    thickness = max(1, font.height // 16)
    x0, x1 = xs[0], xs[0] + max(1, (xs[-1] - xs[0]) * 2 // 3)
    mask = 0
    for x in range(x0, x1 + 1):
        mask |= font.bit(x)
    out = list(font.rows[donor])
    for y in range(mid, min(mid + thickness, font.height)):
        out[y] |= mask
    return out


def accent_mask(font, accented, plain):
    """Lift a diacritic off a letter the font already has.

    `É` minus `E` is the acute on its own, at the height and angle this font
    draws it — which is why the result looks native instead of bolted on.
    """
    a, p = font.index_of(accented), font.index_of(plain)
    if a is None or p is None:
        return None
    return [font.rows[a][y] & ~font.rows[p][y] for y in range(font.height)]


def apply_mask(font, base, mask):
    """Put a diacritic on a base letter, sliding it up if they would collide."""
    b = font.index_of(base)
    if b is None or mask is None:
        return None
    mask_rows = [y for y, row in enumerate(mask) if row]
    base_rows = ink_rows(font, b)
    if not mask_rows or not base_rows:
        return None
    shift = 0
    if mask_rows[-1] >= base_rows[0]:
        # The accent came from a shorter letter: raise it to clear this one.
        shift = mask_rows[-1] - base_rows[0] + 1
        if mask_rows[0] - shift < 0:
            return None  # no room above; better no glyph than a mangled one
    out = list(font.rows[b])
    for y in mask_rows:
        out[y - shift] |= mask[y]
    return out


def half_block(font, which):
    top = [full_row(font)] * (font.height // 2) + [0] * (font.height - font.height // 2)
    if which == "▀":
        return top
    if which == "▄":
        return [0] * (font.height - font.height // 2) + [full_row(font)] * (
            font.height // 2
        )
    if which == "▌":
        mask = 0
        for x in range(font.width // 2):
            mask |= font.bit(x)
        return [mask] * font.height
    raise ValueError(which)


def copy_of(font, donor):
    """Reuse a shape the font already has, unchanged.

    Right for the characters that ARE the same drawing at this resolution: at
    console sizes a single guillemet is the font's own `<`, and a curly quote is
    its own straight one. Inventing a different shape would look worse, not more
    correct.
    """
    g = font.index_of(donor)
    return list(font.rows[g]) if g is not None else None


def dots(font, n):
    """`n` dots in a row, built from the font's own full stop.

    Used for the ellipsis. Taking the dot from the font keeps its size and
    baseline, so `...` sits where the text sits instead of floating.
    """
    g = font.index_of(".")
    if g is None:
        return None
    ys, xs = ink_rows(font, g), ink_cols(font, g)
    if not ys or not xs:
        return None
    dw = xs[-1] - xs[0] + 1
    # Squeeze to one column per dot if three of the font's own will not fit.
    if n * dw + (n - 1) > font.width:
        dw = 1
    gap = 1 if n * dw + (n - 1) <= font.width else 0
    span = n * dw + (n - 1) * gap
    x0 = max(0, (font.width - span) // 2)
    out = [0] * font.height
    for i in range(n):
        left = x0 + i * (dw + gap)
        for y in ys:
            for x in range(left, min(left + dw, font.width)):
                out[y] |= font.bit(x)
    return out


def bullet(font):
    """A round blob at mid-height, sized to the font."""
    r = max(1, min(font.width, font.height) // 6)
    cy, cx = font.height // 2, font.width // 2
    out = [0] * font.height
    for y in range(max(0, cy - r), min(font.height, cy + r + 1)):
        for x in range(max(0, cx - r), min(font.width, cx + r + 1)):
            if (y - cy) ** 2 + (x - cx) ** 2 <= r * r + r:
                out[y] |= font.bit(x)
    return out


def shade(font):
    """A 25% stipple — the light shade block."""
    return [
        sum(font.bit(x) for x in range(font.width) if (x + y) % 2 == 0 and y % 2 == 0)
        for y in range(font.height)
    ]


def arrow(font, which):
    """A geometric arrow: a shaft through the middle and a SOLID head.

    Drawn rather than derived because no font shape resembles an arrow closely
    enough to borrow, and these appear in every key hint the interface prints —
    a blank there reads as a missing instruction.

    The head is a filled triangle. Drawn as an outline it came out as a scatter
    of single pixels around the shaft, because at eight or twelve pixels the two
    diagonals and the shaft land on the same rows with gaps between them: legible
    as a diagram, unreadable as a glyph.
    """
    w, h = font.width, font.height
    out = [0] * h
    cy, cx = h // 2, w // 2
    head = max(1, min(w, h) // 4)

    def put(x, y):
        if 0 <= x < w and 0 <= y < h:
            out[y] |= font.bit(x)

    if which in "\u2190\u2192":  # left, right
        for x in range(1, w - 1):
            put(x, cy)
        tip = 1 if which == "\u2190" else w - 2
        step = 1 if which == "\u2190" else -1
        for i in range(head + 1):
            x = tip + step * i
            for y in range(cy - i, cy + i + 1):
                put(x, y)
        return out

    top, bot = 1, h - 2
    for y in range(top, bot + 1):
        put(cx, y)
    tip = top if which == "\u2191" else bot
    step = 1 if which == "\u2191" else -1
    for i in range(head + 1):
        y = tip + step * i
        for x in range(cx - i, cx + i + 1):
            put(x, y)
    return out


def ghe_upturn(font, upper):
    """\u0433 -> \u0491: the font's own ghe with an upstroke on the right of its bar.

    That IS the letter: Ukrainian ghe with upturn is the Cyrillic ghe plus a tick
    rising from the end of its top bar. Built from the font's own \u0433 so the stroke
    weight matches; refused outright if the letter already reaches the top row,
    because a tick drawn into the row above would collide with the line of text.
    """
    g = font.index_of("\u0413" if upper else "\u0433")
    if g is None:
        return None
    ys = ink_rows(font, g)
    if not ys or ys[0] == 0:
        return None
    top = ys[0]
    cols = [x for x in range(font.width) if font.rows[g][top] & font.bit(x)]
    if not cols:
        return None
    tick = max(1, font.height // 12)
    out = list(font.rows[g])
    for y in range(max(0, top - tick), top):
        out[y] |= font.bit(cols[-1])
    return out


def scaled(font, factor):
    """The same font at an integer multiple of its size.

    A bitmap font cannot be resized smoothly, but doubling every pixel is exact
    and reversible — the shapes are identical, just larger. It is how a console
    at a high resolution gets readable text out of a font that ships in one size
    only: cnxt exists at 9x20 and nothing else, Solarize at 12x29 and nothing
    else, and on a 4K panel both are a smear.

    Not a substitute for a font DRAWN at the larger size — Terminus at 16x32 is
    finer than Terminus at 8x16 doubled — which is why this is only used where
    the alternative is no larger size at all.
    """
    rows = []
    for glyph in font.rows:
        big = []
        for row in glyph:
            wide = 0
            for x in range(font.width):
                if row & font.bit(x):
                    for k in range(factor):
                        # Bit positions are computed against the NEW width below;
                        # collect column indices first, shift once the stride is
                        # known.
                        wide |= 1 << (x * factor + k)
            big.extend([wide] * factor)
        rows.append(big)
    out = Font(font.width * factor, font.height * factor, rows,
               [set(c) for c in font.chars], [list(q) for q in font.seqs])
    # The columns above were numbered from the LEFT as bit 0; the format numbers
    # them from the left as the HIGHEST bit, so flip each row into place.
    for glyph in out.rows:
        for i, row in enumerate(glyph):
            flipped = 0
            for x in range(out.width):
                if row & (1 << x):
                    flipped |= out.bit(x)
            glyph[i] = flipped
    return out


def synthesise(font, ch):
    """Build `ch` out of what this font already has, or return None."""
    if ch in "▀▄▌":
        return half_block(font, ch)
    if ch == "█":
        return [full_row(font)] * font.height
    if ch in "░▒":
        return shade(font)
    if ch in "←↑→↓":
        return arrow(font, ch)
    if ch == "…":
        return dots(font, 3)
    if ch == "•":
        return bullet(font)
    if ch in "ґҐ":
        return ghe_upturn(font, ch == "Ґ")
    # Same drawing at console sizes: borrow rather than invent a worse one.
    borrow = {"‹": "<", "›": ">", "’": "'", "‘": "'", "“": '"', "”": '"',
              "–": "-", "·": "."}
    if ch in borrow:
        return copy_of(font, borrow[ch])
    if ch == "—":
        for donor in "–-":
            g = font.index_of(donor)
            if g is not None:
                return widen_dash(font, g)
        return None
    if ch in "Єє":
        g = font.index_of("C" if ch == "Є" else "c")
        return bar_through(font, g) if g is not None else None
    acutes = {"Á": "A", "Ó": "O", "Ú": "U", "Í": "I", "É": "E"}
    if ch in acutes:
        # Any letter this font already accents will do as the donor pair.
        for pair in (("É", "E"), ("Í", "I"), ("Ó", "O"), ("á", "a"), ("é", "e")):
            if font.covers(pair[0]) and font.covers(pair[1]):
                out = apply_mask(font, acutes[ch], accent_mask(font, *pair))
                if out:
                    return out
        return None
    if ch == "Й":
        for pair in (("й", "и"), ("Ў", "У"), ("ў", "у")):
            if font.covers(pair[0]) and font.covers(pair[1]):
                out = apply_mask(font, "И", accent_mask(font, *pair))
                if out:
                    return out
        return None
    return None


def show(font, glyph, label):
    """Print a glyph as text. A synthesised letter has to be LOOKED at."""
    print(f"    {label}")
    for row in glyph:
        line = "".join(
            "#" if row & font.bit(x) else "." for x in range(font.width)
        )
        print(f"      {line}")


# ------------------------------------------------------------------ the needs


def required_chars(installer_dir):
    """Every non-ASCII character the interface can draw, from the sources.

    Both halves matter: the translations hold the prose, and the Rust literals
    hold the frame — the box lines, the arrows, the list markers.
    """
    need = set()
    for path in sorted(os.listdir(os.path.join(installer_dir, "i18n"))):
        if path.endswith(".toml"):
            text = open(
                os.path.join(installer_dir, "i18n", path), encoding="utf-8"
            ).read()
            for value in re.findall(r'"([^"]*)"', text):
                need |= {c for c in value if ord(c) > 0x7F}
    for root, _, files in os.walk(os.path.join(installer_dir, "src")):
        for name in files:
            if not name.endswith(".rs"):
                continue
            for line in open(os.path.join(root, name), encoding="utf-8"):
                if line.lstrip().startswith(("//", "///", "*")):
                    continue  # comments are for us, not for the console
                for value in re.findall(r'"([^"]*)"', line):
                    need |= {c for c in value if ord(c) > 0x7F}
                    # A character can also be written as an ESCAPE, and those are
                    # exactly the ones that get missed: `\u{26a0}` is eight ASCII
                    # bytes in the source, so a scan for non-ASCII walks straight
                    # past it. That is how a warning sign no console font has
                    # stayed in the interface, printing as a stray digit on the
                    # user's screen, while this check reported everything fine.
                    for esc in re.findall(r"\\u\{([0-9a-fA-F]+)\}", value):
                        code = int(esc, 16)
                        if code > 0x7F:
                            need.add(chr(code))
    return need


def free_slots(font, keep):
    """Glyphs the interface never asks for, emptiest first.

    A glyph mapping nothing at all is given up before one that still draws some
    character, so the font loses as little as possible.
    """
    unused = [
        g
        for g in range(len(font.rows))
        if not (font.chars[g] & keep) and not font.seqs[g]
    ]
    return sorted(unused, key=lambda g: len(font.chars[g]))


def patch(font, need, verbose=True):
    """Give the font every required character it can be taught. Returns a report."""
    keep = need | {chr(c) for c in range(0x20, 0x7F)}
    missing = sorted(c for c in need if not font.covers(c))
    added, refused = [], []
    slots = free_slots(font, keep)
    for ch in missing:
        glyph = synthesise(font, ch)
        if glyph is None:
            refused.append(ch)
            continue
        if not slots:
            refused.append(ch)
            continue
        g = slots.pop(0)
        font.rows[g] = glyph
        font.chars[g] = {ch}
        added.append(ch)
        if verbose:
            show(font, glyph, f"{ch}  (slot {g})")
    return added, refused, missing


def check(fonts, need, installer):
    """Report which offered fonts can draw the whole interface. Exit 1 if any cannot.

    This is the machine-readable version of "is the installer legible?". A
    console can only draw what the loaded font contains, and a missing glyph is
    not an error anywhere — it is a blank on the user's screen. Nothing warns
    you; you find out when a Ukrainian sentence has a hole in it, or a key hint
    turns into whitespace. So it gets checked, not eyeballed.
    """
    print(f"the interface needs {len(need)} non-ASCII characters\n")
    bad = 0
    for path in fonts:
        name = os.path.basename(path).split(".psf")[0]
        try:
            font = read_font(path)
        except Exception as exc:
            print(f"  {name:22} UNREADABLE: {exc}")
            bad += 1
            continue
        missing = sorted(c for c in need if not font.covers(c))
        # The half blocks are used by ONE thing, the donation QR, and the
        # installer asks whether the loaded font has them before drawing it. A
        # font without them is not illegible, it just does not show that code —
        # so it must not be reported as a failure. Terminus is the case: it is
        # OFL with a reserved font name, cannot be patched under its own name,
        # and is handled by the gate instead.
        gated = [c for c in missing if c in "▀▄▌"]
        missing = [c for c in missing if c not in "▀▄▌"]
        size = f"{font.width}x{font.height}"
        if gated and not missing:
            print(f"  {name:22} {size:7} complete (no {''.join(gated)}: the QR is skipped on it)")
            continue
        if missing:
            fixable = [c for c in missing if synthesise(font, c) is not None]
            print(f"  {name:22} {size:7} MISSING {len(missing)}: {''.join(missing)}")
            if fixable:
                print(f"  {'':22} {'':7} (patchable: {''.join(fixable)})")
            bad += 1
        else:
            print(f"  {name:22} {size:7} complete")
    print()
    if bad:
        print(f"{bad} font(s) cannot draw the whole interface.")
        print("Either patch them (this script's default mode) or drop them from")
        print(f"{installer}/src/screens/fontpick.rs — an offered font that cannot")
        print("draw the interface is worse than one fewer choice.")
    else:
        print(f"all {len(fonts)} fonts draw the whole interface.")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("fonts", nargs="+")
    ap.add_argument("--out", help="directory to write patched fonts to")
    ap.add_argument(
        "--installer",
        default=os.path.join(os.path.dirname(__file__), "..", "installer"),
    )
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument(
        "--scale",
        type=int,
        default=1,
        help="write the font at N times its size as well (2 doubles every pixel)",
    )
    ap.add_argument(
        "--check",
        action="store_true",
        help="only report coverage; write nothing. Exits 1 if a font falls short.",
    )
    args = ap.parse_args()
    if not args.check and not args.out:
        ap.error("--out is required unless --check is given")

    need = required_chars(args.installer)
    if args.check:
        return check(args.fonts, need, args.installer)
    print(f"the interface needs {len(need)} non-ASCII characters")
    os.makedirs(args.out, exist_ok=True)

    failures = 0
    for path in args.fonts:
        font = read_font(path)
        name = os.path.basename(path).split(".psf")[0]
        print(f"\n{name}  {font.width}x{font.height}  {len(font.rows)} glyphs")
        if len(font.rows) > CONSOLE_GLYPH_LIMIT:
            print(f"  ! {len(font.rows)} glyphs — the console loads only 512")
        added, refused, missing = patch(font, need, verbose=not args.quiet)
        print(f"  missing {len(missing)}: {''.join(missing) or '(none)'}")
        if added:
            print(f"  added   {len(added)}: {''.join(added)}")
        if refused:
            print(f"  COULD NOT BUILD {len(refused)}: {''.join(refused)}")
            failures += 1
        if args.scale > 1:
            font = scaled(font, args.scale)
            name = f"{name}-x{args.scale}"
        out = os.path.join(args.out, f"{name}.psfu.gz")
        write_font(font, out)
        # Read it back: a font that cannot be re-read is worse than no font.
        written = read_font(out)
        still = sorted(c for c in need if not written.covers(c))
        size = os.path.getsize(out)
        print(f"  -> {out}  ({size} bytes)  " + ("COMPLETE" if not still else f"still missing {''.join(still)}"))
        if still:
            failures += 1
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
