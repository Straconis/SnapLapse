#!/bin/sh
# Turns the SVG artwork into PNG and ICO files. Runs inside a throwaway container:
#   podman run --rm -v "$PWD":/work:Z -w /work docker.io/library/alpine:3.20 sh render.sh
set -e
apk add --no-cache rsvg-convert imagemagick font-inter >/dev/null
mkdir -p out/icons
# App icon PNGs. Each size uses the version drawn for it.
for s in 16 20; do rsvg-convert -w $s -h $s snaplapse-icon-tiny.svg  -o out/icons/snaplapse-$s.png; done
for s in 24 32; do rsvg-convert -w $s -h $s snaplapse-icon-small.svg -o out/icons/snaplapse-$s.png; done
for s in 48 64 128 256 512; do rsvg-convert -w $s -h $s snaplapse-icon.svg -o out/icons/snaplapse-$s.png; done
# Windows .ico with every size Windows asks for
magick out/icons/snaplapse-16.png out/icons/snaplapse-20.png out/icons/snaplapse-24.png \
       out/icons/snaplapse-32.png out/icons/snaplapse-48.png out/icons/snaplapse-64.png \
       out/icons/snaplapse-128.png out/icons/snaplapse-256.png out/snaplapse.ico
# README banner at 2x for sharp display
rsvg-convert -w 2560 -h 800 snaplapse-banner.svg -o out/snaplapse-banner.png
