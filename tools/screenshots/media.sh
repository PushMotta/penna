#!/usr/bin/env bash
# Turns run.sh's captures into the README's media: each still cropped to what Penna shows (the backdrop around it is plain,
# so trimming its colour finds the card), and session.mp4 cut at the tour's marks into GIFs.
#
#   tools/screenshots/media.sh RAW OUT     RAW is run.sh's directory (with themes/ for its themes part), OUT plugin/docs/media
set -euo pipefail

raw=$1
out=$2
mkdir -p "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The box of everything that isn't the backdrop, inside a region: "W H X Y" in screen pixels.
box() { # file region-geometry
  magick "$1" -crop "$2" +repage -fuzz 3% -trim -format "%w %h %X %Y" info: | tr -d '+'
}

# A studio-sized card: below the bar strip, anywhere on the screen.
studio_box() {
  read -r w h x y < <(box "$1" "2560x1380+0+40")
  echo "$w $h $x $((y + 40))"
}

# The popup and the glyph above it, in the screen's top-right corner, with a margin of backdrop.
popup_box() {
  read -r w h x y < <(box "$1" "700x1100+1860+0")
  local m=14
  x=$((x + 1860 - m)); y=0; w=$((w + 2 * m)); h=$((h + m + 6))
  [ $((x + w)) -gt 2560 ] && w=$((2560 - x))
  echo "$w $h $x $y"
}

crop() { # file "w h x y" out
  read -r w h x y <<<"$2"
  magick "$1" -crop "${w}x${h}+${x}+${y}" +repage -strip "$3"
}

# The smallest box holding all of these boxes.
union() {
  awk '{ x2 = $3 + $1; y2 = $4 + $2
         if (NR == 1 || $3 < x) x = $3; if (NR == 1 || $4 < y) y = $4
         if (NR == 1 || x2 > X) X = x2; if (NR == 1 || y2 > Y) Y = y2 }
       END { print X - x, Y - y, x, y }'
}

echo "== Stills"
for f in popup popup-defaults popup-full-speed laptop-popup; do
  [ -f "$raw/$f.png" ] && crop "$raw/$f.png" "$(popup_box "$raw/$f.png")" "$out/$f.png"
done
for f in setup-start setup-pairing setup-calibration setup-load setup-review curves-draft studio studio-edited laptop-studio; do
  [ -f "$raw/$f.png" ] && crop "$raw/$f.png" "$(studio_box "$raw/$f.png")" "$out/$f.png"
done

echo "== The theme GIF"
if [ -d "$raw/themes" ]; then
  frames=()
  for t in current nord catppuccin-latte gruvbox rose-pine tokyo-night; do
    f="$raw/themes/theme-$t.png"
    [ -f "$f" ] || continue
    crop "$f" "$(studio_box "$f")" "$tmp/theme-$t.png"
    magick "$tmp/theme-$t.png" -resize 960x "$tmp/theme-$t-small.png"
    frames+=("$tmp/theme-$t-small.png")
  done
  magick -delay 170 -loop 0 "${frames[@]}" -layers Optimize "$out/themes.gif"
fi

echo "== GIFs from the tour"
start=$(awk '/^RECORDING/ { print $2 }' "$raw/marks.txt")
at() { awk -v n="$1" -v w="$2" -v s="$start" '$3 == "MARK" && $4 == n && $5 == w { printf "%.2f", ($6 - s) / 1000 }' "$raw/marks.txt"; }
gif() { # name "w h x y" width speed
  local from to
  from=$(at "$1" start)
  to=$(at "$1" end)
  read -r w h x y <<<"$2"
  local filters="crop=${w}:${h}:${x}:${y},setpts=PTS/$4,fps=12,scale=$3:-1:flags=lanczos"
  ffmpeg -v error -y -ss "$from" -to "$to" -i "$raw/session.mp4" \
    -vf "$filters,split[a][b];[a]palettegen=stats_mode=diff:max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
    "$out/$1.gif"
}
setup_area=$(for f in setup-start setup-pairing setup-calibration setup-load setup-review; do studio_box "$raw/$f.png"; done | union)
studio_area=$(for f in studio studio-edited; do studio_box "$raw/$f.png"; done | union)
popup_area=$(for f in popup popup-full-speed; do popup_box "$raw/$f.png"; done | union)
gif setup "$setup_area" 960 2
gif studio "$studio_area" 960 1
gif popup "$popup_area" "$(awk '{ print $1 }' <<<"$popup_area")" 1

ls -la "$out"
