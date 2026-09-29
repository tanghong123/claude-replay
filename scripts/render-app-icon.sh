#!/bin/sh
# Render the monitor's Home Screen and install icons (#320) from their one source,
# claude-monitor/src/icons/app-icon.svg: the tab icon's green list mark, full-bleed and square,
# because a phone masks the corners itself and draws any transparency as black. Needs ImageMagick
# (`magick`). The PNGs are committed; re-run this after editing the SVG, and the tests in
# claude-monitor/src/ui.rs check each PNG's size and that it is opaque.
set -eu
dir="$(cd "$(dirname "$0")/.." && pwd)/claude-monitor/src/icons"
for size in 180 192 512; do
  case "$size" in
    180) out="$dir/apple-touch-icon.png" ;;
    *) out="$dir/icon-$size.png" ;;
  esac
  # -density high enough that the vector is rasterised above the target size, then resized down;
  # -alpha off and PNG24 write an RGB image with no alpha channel at all.
  magick -background '#2e7d55' -density 1600 "$dir/app-icon.svg" -resize "${size}x${size}" \
    -alpha remove -alpha off -strip "PNG24:$out"
  echo "rendered $out"
done
