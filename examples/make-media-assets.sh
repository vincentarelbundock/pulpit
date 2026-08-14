#!/bin/sh
# Regenerate every asset examples/beamer.tex references.
#
# Deterministic: the same tools and the same script produce the same bytes,
# so the checked-in assets can be rebuilt and diffed.
#
# Requires: ImageMagick 7 (magick), ffmpeg, zip.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
out="$here/media-assets"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$out"

GIF_W=480
GIF_H=270
FRAMES=30
# An even count, so the two horizontal reversals (at t=0 and t=1/2) both land
# on a floor contact. An odd count turns the ball around at the apex against
# one wall and at the floor against the other, and the loop reads as two
# different animations spliced together.
BOUNCES=4

# --- bouncing.gif --------------------------------------------------------
# A ball under gravity, bouncing off the floor and the right wall. Frame
# geometry is computed in awk so the animation is a formula, not a fixture.
i=0
while [ "$i" -lt "$FRAMES" ]; do
	set -- $(awk -v i="$i" -v n="$FRAMES" -v w="$GIF_W" -v h="$GIF_H" -v b="$BOUNCES" 'BEGIN {
		r = 26
		# One full horizontal traverse and back over the loop, so frame N
		# meets frame 0 seamlessly.
		#
		# At a constant speed, reversing at each wall. A cosine ease was the
		# obvious way to write this and the wrong one: it brings the ball to
		# a standstill at both ends of the traverse, and one of those ends is
		# the loop seam — so the animation appeared to stop for a fifth of a
		# second before starting again. Nothing in the file said "pause";
		# the ball was simply crawling.
		t = i / n
		u = 2 * t
		if (u > 1) u = 2 - u
		x = r + (w - 2 * r) * u
		# Bounces are inverted parabolas. The ball touches down exactly
		# where it turns around, so frame n-1 hands off to frame 0 the
		# same way every other frame hands off to the next.
		p = (t * b) % 1
		bounce = 4 * p * (1 - p)
		y = (h - r - 8) - (h - 2 * r - 40) * bounce
		printf "%d %d %d", x, y, r
	}')
	x=$1
	y=$2
	r=$3
	magick -size "${GIF_W}x${GIF_H}" xc:'#101820' \
		-fill '#1b2836' -draw "rectangle 0,$((GIF_H - 8)) ${GIF_W},${GIF_H}" \
		-fill '#0b1119' -draw "ellipse ${x},$((GIF_H - 6)) $((r - 4)),4 0,360" \
		-fill '#e6544b' -draw "circle ${x},${y} $((x + r)),${y}" \
		-fill '#f4a9a3' -draw "circle $((x - r / 3)),$((y - r / 3)) $((x - r / 3 + r / 4)),$((y - r / 3))" \
		"$work/gif-$(printf '%03d' "$i").png"
	i=$((i + 1))
done

# OptimizeTransparency rewrites the unchanged pixels of each frame as
# transparent and sets a matching disposal method, which both shrinks the
# file and exercises GIF disposal handling in the decoder.
#
# -dispose must be set before the frames are read, or it is silently dropped,
# and it must be None: "previous" restores the canvas to its pre-frame state,
# so the transparent pixels of frame N+1 uncover frame N-1 instead of frame N
# and the accumulated image comes apart — worst of all at the loop seam.
magick -delay 4 -loop 0 -dispose none "$work"/gif-*.png \
	-layers OptimizeTransparency \
	"$out/bouncing.gif"

# The still poster is the first frame, at the same size as the GIF.
magick "$work/gif-000.png" "$out/bouncing-still.png"

# --- clip.mp4 ------------------------------------------------------------
# Six silent seconds of colour bars with a moving pattern and a running
# timestamp, so it is obvious the video is playing and where it is.
ffmpeg -y -loglevel error -nostdin \
	-f lavfi -i "testsrc2=size=640x360:rate=25:duration=6" \
	-vf "drawtext=text='pulpit %{pts\\:hms}':x=16:y=16:fontsize=28:fontcolor=white:box=1:boxcolor=black@0.5" \
	-an -c:v libx264 -pix_fmt yuv420p -preset veryslow -crf 30 \
	-movflags +faststart -fflags +bitexact -flags +bitexact \
	"$out/clip.mp4"

# --- poster.png ----------------------------------------------------------
ffmpeg -y -loglevel error -nostdin -i "$out/clip.mp4" -frames:v 1 \
	"$work/poster-raw.png"
magick "$work/poster-raw.png" -resize 640x360! \
	-fill '#000000C0' -draw "rectangle 0,300 640,360" \
	-fill white -pointsize 26 -annotate +16+340 'poster for run:clip.mp4' \
	"$out/poster.png"

# --- balls-poster.png ----------------------------------------------------
# A static impression of bouncing-balls.html, in the page's own palette.
magick -size 1280x720 xc:'#101820' \
	-fill '#e6544b' -draw 'circle 300,520 300,458' \
	-fill '#f0a202' -draw 'circle 520,600 520,556' \
	-fill '#3fb27f' -draw 'circle 720,470 720,398' \
	-fill '#4f9bd9' -draw 'circle 930,590 930,538' \
	-fill '#b06ad9' -draw 'circle 1090,500 1090,466' \
	-fill '#e8dfc7' -draw 'circle 430,300 430,272' \
	-fill '#8a97a8' -pointsize 30 -annotate +24+690 \
	'click a ball to kick it - click empty space to add one' \
	-fill '#dfe6ef' -pointsize 44 -annotate +24+70 'Interactive HTML overlay' \
	"$out/balls-poster.png"

# --- bouncing-balls.html -------------------------------------------------
# The page is served from its own directory beside the document, so it is
# simply copied in rather than packed into a bundle.
cp "$here/bouncing-balls.html" "$out/bouncing-balls.html"

ls -l "$out"
