#!/bin/sh
# Install the fixture's dependencies, once, for every task to share.
#
# `feldspar agent eval` copies the fixture into a temporary directory per task,
# and the project's checks (`npm run typecheck`, `npm test`) need
# `node_modules`. Copying it ten times would be a gigabyte and a minute per
# task, so it is installed **beside** the fixture and reached through a symlink:
# the harness copies a symlink as a symlink, so every task's copy shares this one
# install and still has its own source tree.
#
# Run this once before the first eval, and again after changing the fixture's
# `package.json`. Both paths are git-ignored.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
modules="$here/node_modules"
fixture="$here/fixture"

# A previous run left a symlink; npm would install through it.
if [ -L "$fixture/node_modules" ]; then
  rm "$fixture/node_modules"
fi
# A first run may have installed into the fixture itself.
if [ -d "$fixture/node_modules" ] && [ ! -d "$modules" ]; then
  mv "$fixture/node_modules" "$modules"
fi

mkdir -p "$modules"
ln -sfn "$modules" "$fixture/node_modules"
( cd "$fixture" && npm install )

# The baseline every task starts from must be green, or every task would be
# scored on failures it inherited.
( cd "$fixture" && npm run --silent typecheck && npm test --silent )
echo "fixture ready: $fixture (node_modules -> $modules)"
