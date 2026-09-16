# Third-party notices

The deck embeds bitmap fonts whose licences require their notices to travel with
them. This file is those notices. It is not a summary — the terms below are quoted
verbatim from the upstream sources, because a paraphrase of a licence is not the
licence.

Everything else the deck links (libsndfile, ALSA, and the Rust crates in
`Cargo.toml`) is used as an ordinary dependency under its own terms and is not
reproduced here. Fonts are different: the glyph data is compiled *into* the binary.

## What is embedded, and where it came from

`src/display/paint.rs` uses two faces out of the `u8g2-fonts` crate:

- `u8g2_font_b12_t_japanese3`
- `u8g2_font_b16_t_japanese3`

The crate is MIT/Apache-2.0; **the font data is not, and is licensed per group.**
These two are the [efont project](http://openlab.ring.gr.jp/efont/)'s Unicode
bitmap fonts, subset to u8g2's `japanese3` map. efont merges many sources, so
"which licence" is a per-glyph question — and it was answered by one, on
2026-09-16, rather than assumed.

**Method, so the table can be rechecked rather than believed.** efont's source
package ships each component font as a separate `.hex` and states the merge order
in `Makefile.in`; `tools/hexmerge` assigns into a hash as it reads, so **the last
file named wins**. Applying that order to every codepoint in `japanese1.map`,
`japanese2.map` and `japanese3.map` attributes each one to a source, and every
attribution below was then confirmed **bit-for-bit against the glyph u8g2 actually
ships**. The analysis lives in
[issue #10](https://github.com/tamatebox/deck-pi/issues/10).

| Source | Glyphs reaching `japanese3` | Licence |
|---|---|---|
| shinonome 0.9.6 (`shnmk12`, `shnm6x12r`, `shnmk16`) | 3698 at 12 px, 3696 at 16 px | Public Domain |
| `K12-[12].bdf` 0.15, Toshiyuki Imamura | 53 at 12 px | Public Domain |
| `jiskan16-2000-[12].bdf` 1.03, Imamura and HANATAKA Shinya | 52 at 16 px | Public Domain |
| `6x12` / `8x13` (ucs-fonts, Markus Kuhn) | 95 at 12 px, 1 at 16 px | Public Domain |
| `etl16-unicode` | 93 at 16 px | Public Domain |
| `taipei16` (GNU intlfonts) | 10 at each size | Public Domain |
| efont's own additions (`f12_add`, `f16_add`, `h12_add`, `h16_add`) | 1 at 12 px, 13 at 16 px | BSD-style, below |
| **baekmuk** (`gulim12`, `dotum16`) | **12 at 12 px, 9 at 16 px** | Below |
| **Academia Sinica** (`gb16fs`) | **1 at each size** (`吞`) | Below |

The two in bold are the reason this file exists. At 12 px they are
`俱 剝 噓 姸 屛 幷 瘦 繫 ￠ ￡ ￢ ￦` and `吞`; at 16 px, `俱 剝 噓 姸 屛 幷 瘦 繫 ￢`
and `吞`. **`japanese1` and `japanese2` contain none of them** — those two subsets
are Public Domain and efont's own throughout, at both sizes. The deck uses
`japanese3` for its coverage, which is why these notices apply.

One nuance, recorded rather than smoothed over: baekmuk's permission notice names
"the 4 Baekmuk **truetype outline** fonts", and efont ships it as the licence for
the bitmap `gulim`/`dotum` BDFs. That is efont's reading, carried in Debian main
since 2001; it is a reading, not a grant written for bitmaps.

---

## /efont/ The Electronic Font Open Laboratory

```
(c) Copyright 2000-2001 /efont/ The Electronic Font Open Laboratory.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the distribution.
3. Neither the name of the team nor the names of its contributors
   may be used to endorse or promote products derived from this font
   without specific prior written permission.

THIS FONT IS PROVIDED BY THE TEAM AND CONTRIBUTORS ``AS IS'' AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL THE TEAM OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR
BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE
OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS FONT, EVEN
IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Baekmuk (`gulim12`, `dotum16`)

```
(c) Copyright 1986-2000, Hwan Design Inc.

You are hereby granted permission under all Hwan Design propriety rights
to use, copy, modify, sublicense, sell, and redistribute the 4 Baekmuk
truetype outline fonts for any purpose and without restriction;
provided, that this notice is left intact on all copies of such fonts
and that Hwan Design Int.'s trademark is acknowledged as shown below
on all copies of the 4 Baekmuk truetype fonts.

BAEKMUK BATANG is a registered trademark of Hwan Design Inc.
BAEKMUK GULIM is a registered trademark of Hwan Design Inc.
BAEKMUK DOTUM is a registered trademark of Hwan Design Inc.
BAEKMUK HEADLINE is a registered trademark of Hwan Design Inc.
```

## The Institute of Software, Academia Sinica (`gb16fs`)

```
Copyright (C) 1988  The Institute of Software, Academia Sinica.

Correspondence Address:  P.O.Box 8718, Beijing, China 100080.

Permission to use, copy, modify, and distribute this software and
its documentation for any purpose and without fee is hereby granted,
provided that the above copyright notices appear in all copies and
that both those copyright notices and this permission notice appear
in supporting documentation, and that the name of "the Institute of
Software, Academia Sinica" not be used in advertising or publicity
pertaining to distribution of the software without specific, written
prior permission.  The Institute of Software, Academia Sinica,
makes no representations about the suitability of this software
for any purpose.  It is provided "as is" without express or
implied warranty.

THE INSTITUTE OF SOFTWARE, ACADEMIA SINICA, DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE, INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS, IN NO EVENT SHALL THE INSTITUTE OF
SOFTWARE, ACADEMIA SINICA, BE LIABLE FOR ANY SPECIAL, INDIRECT OR
CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION
WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

## The Public Domain sources

No notice is required for these, and they are named because attribution that is not
owed is still worth giving: shinonome (maintained by /efont/), `K12` and
`jiskan16-2000` by Toshiyuki Imamura and HANATAKA Shinya, the `ucs-fonts` family by
Markus Kuhn, `etl16-unicode`, and `taipei16` from GNU intlfonts.

**`naga10` is deliberately absent.** It is the 10 px face's source, "freely usable,
but restricted", and its `README.naga10` is in Japanese and has not been read. The
deck does not use `b10_t_japanese*` for that reason.
