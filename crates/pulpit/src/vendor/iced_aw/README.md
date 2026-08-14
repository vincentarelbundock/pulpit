# iced_aw — colour picker and time picker

Vendored source. Not written here, not maintained here, not this crate's to
restyle.

| | |
|---|---|
| Upstream | <https://github.com/iced-rs/iced_aw> |
| Version | 0.14.1 (crates.io, published 2026-04-27) |
| Licence | MIT — see `LICENSES/ICED_AW-LICENSE` at the repository root |
| Copyright | 2020 Kaiden42, and the iced_aw contributors |

## Why a copy rather than a dependency

`iced_aw` is one crate carrying twenty widgets, versioned against iced. Two of
those widgets are wanted here: the colour picker, for the palette editor in
settings, where a hex field is a poor way to choose a colour; and the time
picker, for dialling a cue on the clock.

Depending on the crate to get them would mean:

* an iced upgrade waits on `iced_aw` releasing against it — its `main` branch
  has already moved to iced 0.15, so a 0.14 line is a line that stops moving;
* `iced_fonts` comes along unconditionally, to load an icon *font*, in an
  application that draws every icon as SVG precisely so glyphs cannot land at
  a different weight than the drawings beside them;
* the published crate defaults to `full`, so the default build also carries
  chrono's date machinery, `num-format`, and seventeen widgets nobody calls.

Copying two widgets costs a one-time port and nothing afterwards.

## What is here

```
core/       colour and time arithmetic, the clock-face geometry, overlay
            position — only the modules the two pickers use
style/      the Catalog/Style traits and the default palettes for both
widget/     the two widgets and their overlays
glyphs.rs   (in the parent directory) the button labels, in place of the
            upstream icon font
```

## Changes made to upstream

Kept deliberately small. Every one is marked `VENDOR:` in the source where it
is not purely mechanical.

1. **Module paths.** `crate::core::…`, `crate::style::…`, `crate::widget::…`
   and `crate::time_picker::…` became `crate::vendor::iced_aw::…`.
2. **Icon font.** `crate::iced_aw_font::advanced_text::{cancel, ok, up_open,
   down_open}` became `crate::vendor::iced_aw::glyphs::{…}`, which returns the
   same `(content, font, shaping)` triple with words and the default font
   instead of private-use codepoints from a bundled `font.ttf`.
3. **Edition.** One let-chain in `widget/overlay/color_picker.rs` was split
   into nested `if`s: let-chains are Rust 2024 syntax and this workspace is on
   the 2021 edition.
4. **Formatting.** `cargo fmt` was run once, under this workspace's edition,
   so `make lint` passes. This is the only wholesale difference from upstream
   and it is reproducible.

Upstream's own unit tests came with the files and run in this crate's test
suite (`cargo test -p pulpit vendor::`) — 170 of them, and they are
the check that the port did not break anything.

## Re-vendoring a later version

```sh
cargo download iced_aw==<version>          # or fetch the .crate by hand
cp -r <crate>/src/{core,style,widget}/…     # the files listed above
# then, over the copied tree:
sed -i -e 's|crate::iced_aw_font::advanced_text|crate::vendor::iced_aw::glyphs|g' \
       -e 's|\bcrate::core::|crate::vendor::iced_aw::core::|g' \
       -e 's|\bcrate::style::|crate::vendor::iced_aw::style::|g' \
       -e 's|\bcrate::widget::|crate::vendor::iced_aw::widget::|g' \
       -e 's|^use crate::{|use crate::vendor::iced_aw::{|' \
       -e 's|^pub use crate::{|pub use crate::vendor::iced_aw::{|' *.rs
cargo fmt --all && cargo test -p pulpit vendor::
```

The `mod.rs` files in this tree are written here, not copied: upstream's are
full of `#[cfg(feature = …)]` for widgets that were not taken.
