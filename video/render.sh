#!/bin/sh
# Renders the demo film in the Playwright image, then encodes it with ffmpeg on the host.
#   video/render.sh preview 3 22 41
#   video/render.sh film
set -e
cd "$(dirname "$0")/.."
mkdir -p video/out/frames
docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD":/site:ro -v "$PWD/video/out":/out \
  -v "$PWD/video/render.mjs":/mnt/render.mjs:ro mcr.microsoft.com/playwright:v1.56.1-noble sh -c '
    mkdir -p /tmp/app && cd /tmp/app && cp /mnt/render.mjs . && npm init -y >/dev/null \
    && npm i --silent --no-audit --no-fund --ignore-scripts playwright@1.56.1 >/dev/null 2>&1
    (cd /site && python3 -m http.server 8000 >/dev/null 2>&1 &)
    sleep 1
    node render.mjs "$@"' sh "$@"
if [ "$1" = film ]; then
  ffmpeg -y -loglevel error -framerate 30 -i video/out/frames/%05d.jpg -vf scale=out_range=tv,format=yuv420p -c:v libx264 -preset slow -crf 20 -movflags +faststart video/out/deed-demo.mp4
  rm -rf video/out/frames
  ls -la video/out/deed-demo.mp4
fi
