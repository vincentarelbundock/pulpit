# A folder of images, as a document

Ten pages of Pepper&Carrot Book 1, so there is something to point at when
trying the image-directory tier (`SPEC-images.md`):

```sh
pulpit examples/images
pulpit examples/images/page-04.jpg    # opens the same folder, on page 4
```

The directory *is* the document. Pages are the supported image files directly
inside it, in natural name order; this README is not one of them, because the
supported set is decided by extension alone. The pages are not all the same
shape either — `page-01.jpg` is 1367×1778 and `page-03.jpg` is 1294×1688 —
which is the mixed-geometry case the overview grid and aspect fit already
handle.

Drop another `.jpg` in here, or overwrite one in place, and the folder reloads
the way a rebuilt deck does.

## Credit

> "Pepper&Carrot" by David Revoy, licensed under Creative Commons
> Attribution 4.0.

That is the credit line the author asks for, and it travels with these files.
Source: <https://www.peppercarrot.com>. Licence deed:
<https://creativecommons.org/licenses/by/4.0/>. The author's own guidance is
at <https://www.peppercarrot.com/en/license/>, and the verbatim licence text
is in `LICENSES/PEPPERCARROT-LICENSE.txt`.
