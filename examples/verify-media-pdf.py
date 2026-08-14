#!/usr/bin/env python3
"""Check that the example deck carries what pulpit reads.

The deck declares its media the way pdfpc and Impressive decks already do —
`\\href{run:file?params}` around a poster — and hyperref turns that into a
/Launch action whose /F is the file plus its query string. This checks that
every one of them survived the LaTeX round trip verbatim, since `&` is an
alignment tab in TeX unless the deck fixes its catcode, and that each file
named actually sits beside the document.

Requires pypdf.  Usage: python3 verify-media-pdf.py combined.pdf
"""

import sys
from pathlib import Path

from pypdf import PdfReader

EXPECTED = {
    "media-assets/bouncing.gif?autostart&loop",
    "media-assets/clip.mp4?autostart&mute",
    "media-assets/clip.mp4?loop&mute",
    "media-assets/bouncing-balls.html",
}


def launch_targets(reader):
    """Every file a /Launch action names, with the page it is on."""
    for number, page in enumerate(reader.pages, start=1):
        for annotation in page.get("/Annots") or []:
            annotation = annotation.get_object()
            action = annotation.get("/A")
            if action is None:
                continue
            action = action.get_object()
            if action.get("/S") == "/Launch":
                target = action.get("/F")
                if target is None:
                    continue
                if hasattr(target, "get_object"):
                    target = target.get_object()
                    if hasattr(target, "get"):
                        target = target.get("/F", target)
                yield number, str(target)
            elif "/URI" in action:
                # A `run:` written straight into a /URI action is equally
                # valid; pulpit reads both.
                uri = str(action["/URI"])
                if uri.startswith("run:"):
                    yield number, uri[len("run:") :]


def main(path):
    reader = PdfReader(path)
    found = {}
    for number, target in launch_targets(reader):
        found.setdefault(target, []).append(number)

    print(f"{path}: {len(reader.pages)} pages")
    print("\nMedia links found:")
    for target, pages in sorted(found.items()):
        print(f"  {target}  (pages {', '.join(map(str, pages))})")

    problems = []
    for expected in sorted(EXPECTED):
        if expected not in found:
            problems.append(f"missing or mangled: {expected!r}")

    # The reveal-continuity frame repeats one link on three physical pages.
    repeated = {target: pages for target, pages in found.items() if len(pages) > 1}
    if not repeated:
        problems.append("no link repeats across pages; the \\pause frame is not doing its job")
    else:
        print("\nRepeated across reveal steps (one overlay, one session):")
        for target, pages in sorted(repeated.items()):
            print(f"  {target}  on pages {', '.join(map(str, pages))}")

    print("\nAssets beside the document:")
    directory = Path(path).resolve().parent
    for target in sorted(found):
        name = target.split("?", 1)[0]
        asset = directory / name
        state = "ok" if asset.is_file() else "MISSING"
        if state == "MISSING":
            problems.append(f"{name} is referenced but not present")
        print(f"  {name}  {state}")

    if problems:
        print("\nProblems:")
        for problem in problems:
            print(f"  - {problem}")
        return 1
    print("\nAll expected media links and assets are present.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "combined.pdf"))
