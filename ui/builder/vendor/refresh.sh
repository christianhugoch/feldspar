#!/usr/bin/env bash
#
# Refresh ui/builder/vendor/ and ui/builder/public/ from a Saltcorn 1 checkout.
# See vendor/README.md for what is copied and why.
#
#   ui/builder/vendor/refresh.sh ~/saltcorn
#
# The checkout must be at **the same commit ui/saltcorn-ui/vendor/ was taken
# at**, and this script refuses any other (TODO "The builder" §1): the builder
# writes the layouts the vendored renderers read, and the two must never come
# from two versions of Saltcorn. To move both, run ui/saltcorn-ui/vendor/refresh.sh
# first and this one after it, from the one checkout.
#
# No build is needed: nothing here is required from the checkout, only copied.
#
# Every copied .js, .css and .html file gets a header naming its upstream path
# and the v1 version it was taken at. Nothing else about a file is changed: an
# edit to a vendored file is a fork of it, and belongs in `src/` instead.
set -euo pipefail

V1="${1:?usage: vendor/refresh.sh <saltcorn checkout>}"
V1="$(cd "${V1}" && pwd)"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="${V1}/packages"

VERSION="$(node -p "require('${PKG}/saltcorn-builder/package.json').version")"
COMMIT="$(git -C "${V1}" rev-parse --short=10 HEAD)"

# The commit the renderers were taken at, from a header ui/saltcorn-ui's own
# refresh.sh wrote: "// at @saltcorn/data <version> (saltcorn/saltcorn <commit>). …"
RENDERERS="${HERE}/../saltcorn-ui/vendor/saltcorn-markup/builder.ts"
RENDERERS_COMMIT="$(sed -n '2s/.*(saltcorn\/saltcorn \([0-9a-f]*\)).*/\1/p' "${RENDERERS}")"
if [[ -z "${RENDERERS_COMMIT}" ]]; then
  echo "error: could not read the vendored renderers' commit from ${RENDERERS}" >&2
  exit 1
fi
if [[ "${COMMIT}" != "${RENDERERS_COMMIT}" ]]; then
  echo "error: ${V1} is at ${COMMIT}, but ui/saltcorn-ui/vendor/ was taken at ${RENDERERS_COMMIT}." >&2
  echo "The builder and the renderers it writes for must come from one commit:" >&2
  echo "check out ${RENDERERS_COMMIT}, or refresh ui/saltcorn-ui/vendor/ from ${COMMIT} first." >&2
  exit 1
fi
# A commit is only a version if the files are the commit's.
DIRTY="$(git -C "${V1}" status --porcelain -- packages/saltcorn-builder/src packages/server/public/ckeditor \
  packages/server/public/saltcorn-builder.css packages/server/public/fonticonpicker.react.css \
  packages/server/public/assets)"
if [[ -n "${DIRTY}" ]]; then
  echo "error: ${V1} has uncommitted changes in the files this script copies:" >&2
  echo "${DIRTY}" >&2
  exit 1
fi

V1_LINE="@saltcorn/builder ${VERSION} (saltcorn/saltcorn ${COMMIT})"

header() { # <comment-open> <comment-close> <upstream path>
  printf '%s Vendored from Saltcorn 1: %s\n%s at %s. Do not edit; see ui/builder/vendor/README.md.%s\n' \
    "$1" "$3" "$1" "${V1_LINE}" "${2:+ $2}"
}

copy_with_header() { # <src> <dest> <upstream path>
  local src="$1" dest="$2" upstream="$3"
  mkdir -p "$(dirname "${dest}")"
  # A byte-order mark is only one at the start of a file; after a header it
  # would be a stray character (and, in CSS, part of the first selector).
  case "${dest}" in
    *.js) { header "//" "" "${upstream}"; sed '1s/^\xEF\xBB\xBF//' "${src}"; } >"${dest}" ;;
    *.css) { header "/*" "*/" "${upstream}"; sed '1s/^\xEF\xBB\xBF//' "${src}"; } >"${dest}" ;;
    *.html) { header "<!--" "-->" "${upstream}"; cat "${src}"; } >"${dest}" ;;
    *) cp "${src}" "${dest}" ;;
  esac
}

copy_tree() { # <upstream dir, relative to packages/> <dest dir>
  local from="$1" to="$2"
  while IFS= read -r f; do
    copy_with_header "${PKG}/${from}/${f}" "${to}/${f}" "packages/${from}/${f}"
  done < <(cd "${PKG}/${from}" && find . -type f | sed 's|^\./||' | sort)
}

rm -rf "${HERE}/vendor/saltcorn-builder" "${HERE}/public"

# --- the builder's source: all of it ------------------------------------------
copy_tree saltcorn-builder/src "${HERE}/vendor/saltcorn-builder"

# --- the browser assets v1's builder page links (saltcorn-markup/builder.ts) ---
copy_with_header "${PKG}/server/public/saltcorn-builder.css" "${HERE}/public/saltcorn-builder.css" \
  "packages/server/public/saltcorn-builder.css"
copy_with_header "${PKG}/server/public/fonticonpicker.react.css" "${HERE}/public/fonticonpicker.react.css" \
  "packages/server/public/fonticonpicker.react.css"
# The icon picker's font, which fonticonpicker.react.css names by relative URL.
# Binaries (and an SVG font, whose XML declaration must come first) cannot carry
# a header; README lists where they came from.
for f in fontIconPicker.svg fontIconPicker.ttf fontIconPicker.woff; do
  mkdir -p "${HERE}/public/assets"
  cp "${PKG}/server/public/assets/${f}" "${HERE}/public/assets/${f}"
done
# CKEditor 4, whole: it loads its plugins, skins and languages from beside
# ckeditor.js at run time, so it is served as a directory, not bundled.
copy_tree server/public/ckeditor "${HERE}/public/ckeditor"

echo "vendored ${V1_LINE} into ${HERE}"
