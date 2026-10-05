#!/bin/sh
# Re-records assets/demo.gif with vhs in Docker, in the made-up repository from make-repo.sh.
# Usage: assets/demo/record.sh LINUX_GITTY_BINARY  (built for the Docker host's architecture,
# e.g. the gitty binary from a release's Linux archive). Run from the repo root.
set -eu
bin=$1
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
cp "$here/demo.tape" "$here/make-repo.sh" "$work/"
cp "$bin" "$work/gitty"
mkdir "$work/out"
docker build -q -t gitty-vhs "$here" >/dev/null
docker run --rm -v "$work":/vhs -w /vhs --entrypoint sh gitty-vhs -c \
  'install -m755 gitty /usr/local/bin/gitty && sh make-repo.sh /tmp/skylark >/dev/null && vhs demo.tape'
cp "$work/out/demo.gif" "$here/../demo.gif"
rm -rf "$work"
echo "wrote assets/demo.gif"
