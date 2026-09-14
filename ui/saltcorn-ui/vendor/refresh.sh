#!/usr/bin/env bash
#
# Refresh ui/saltcorn-ui/vendor/ and ui/saltcorn-ui/public/ from a Saltcorn 1
# checkout. See vendor/README.md for what is copied and why.
#
#   ui/saltcorn-ui/vendor/refresh.sh ~/saltcorn [~/any-bootstrap-theme]
#
# The checkout must be *built* (`npm install && npm run tsc` at its root): the
# last step requires v1's own CommonJS entry points to record the export names
# a v1 `require` answers with (`vendor/v1-exports.json`), which is what
# `sc-viewpattern`'s `bundle_shape` test holds the bundle to.
#
# Every copied text file gets a header naming its upstream path and the v1
# version it was taken at. Nothing else about a file is changed: an edit to a
# vendored file is a fork of it, and belongs in `src/` instead.
set -euo pipefail

V1="${1:?usage: vendor/refresh.sh <saltcorn checkout> [any-bootstrap-theme checkout]}"
V1="$(cd "${V1}" && pwd)"
THEME="${2:-${V1}/../any-bootstrap-theme}"
THEME="$(cd "${THEME}" && pwd)"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="${V1}/packages"

VERSION="$(node -p "require('${PKG}/saltcorn-data/package.json').version")"
COMMIT="$(git -C "${V1}" rev-parse --short=10 HEAD)"
THEME_VERSION="$(node -p "require('${THEME}/package.json').version")"
THEME_COMMIT="$(git -C "${THEME}" rev-parse --short=10 HEAD)"

# --- the rendering half of @saltcorn/data, and its pure dependencies ----------
DATA_FILES=(
  base-plugin/viewtemplates/list.ts
  base-plugin/viewtemplates/show.ts
  base-plugin/viewtemplates/edit.ts
  base-plugin/viewtemplates/feed.ts
  base-plugin/viewtemplates/filter.ts
  base-plugin/viewtemplates/listshowlist.ts
  base-plugin/viewtemplates/viewable_fields.ts
  base-plugin/types.ts
  base-plugin/fieldviews.ts
  base-plugin/fileviews.ts
  plugin-helper.ts
  viewable_fields.ts
  models/form.ts
  models/fieldrepeat.ts
  models/expression.ts
  models/layout.ts
  # Library.resolveSegment and suitableFor; its `db` is src/shims/library-db.ts.
  models/library.ts
  utils.ts
  # getState().evaluator: what a formula in a view is evaluated with.
  evaluator.ts
  diagram/node_extract_utils.ts
  diagram/nodes/node.ts
  diagram/nodes/page_node.ts
  diagram/nodes/table_node.ts
  diagram/nodes/trigger_node.ts
  diagram/nodes/view_node.ts
  diagram/nodes/dummy_node.ts
  tests/mocks.ts
  # v1's own answer to "run @saltcorn/data without its server" (the mobile
  # build's webpack aliases), used here for the same imports.
  mobile-mocks/saltcorn/plugin-testing.ts
  mobile-mocks/npm/dockerode.ts
  mobile-mocks/npm/xml2js.ts
  mobile-mocks/node/fs-extra.ts
)
MARKUP_FILES=(
  builder.ts emergency_layout.ts extra_tags.ts form.ts generated_tags.ts
  helpers.ts index.ts internal.ts layout.ts layout_utils.ts mjml-layout.ts
  mjml-tags.ts mktag.ts table.ts tabs.ts tags.ts types.ts workflow.ts
)
COMMON_CODE_FILES=(
  index.ts
  relations/relation.ts
  relations/relation_helpers.ts
  relations/relation_types.ts
  relations/relations_finder.ts
)
DB_COMMON_FILES=(internal.ts dbtypes.ts)
# common_types, generators and base_types have runtime code the above calls; the
# model-abstracts are types plus the `instanceOf*` guards markup and diagram use.
TYPES_FILES=(common_types.ts generators.ts base_types.ts)
while IFS= read -r f; do TYPES_FILES+=("model-abstracts/${f}"); done \
  < <(cd "${PKG}/saltcorn-types/model-abstracts" && ls -1 *.ts | grep -v '\.test\.ts$')
PLAIN_DATE_FILES=(index.ts)

# --- the browser assets (§9) -------------------------------------------------
# upstream (relative to packages/) -> public/ path
PUBLIC_FILES=(
  "server/public/jquery-3.6.0.min.js:jquery-3.6.0.min.js"
  "server/public/saltcorn-common.js:saltcorn-common.js"
  "server/public/saltcorn.js:saltcorn.js"
  "server/public/saltcorn.css:saltcorn.css"
  "saltcorn-sbadmin2/public/bootstrap.bundle.min.js:bootstrap.bundle.min.js"
  "saltcorn-sbadmin2/public/fontawesome-free/css/all.min.css:fontawesome-free/css/all.min.css"
)

header() { # <comment-open> <comment-close> <upstream path> <version line>
  printf '%s Vendored from Saltcorn 1: %s\n%s at %s. Do not edit; see ui/saltcorn-ui/vendor/README.md.%s\n' \
    "$1" "$3" "$1" "$4" "${2:+ $2}"
}

copy_with_header() { # <src> <dest> <upstream path> <version line>
  local src="$1" dest="$2" upstream="$3" version="$4"
  mkdir -p "$(dirname "${dest}")"
  case "${dest}" in
    *.ts | *.js) { header "//" "" "${upstream}" "${version}"; cat "${src}"; } >"${dest}" ;;
    *.css) { header "/*" "*/" "${upstream}" "${version}"; cat "${src}"; } >"${dest}" ;;
    *) cp "${src}" "${dest}" ;;
  esac
}

V1_LINE="@saltcorn/data ${VERSION} (saltcorn/saltcorn ${COMMIT})"

find "${HERE}/vendor" -mindepth 1 -maxdepth 1 -type d -exec rm -rf {} +
rm -rf "${HERE}/public"

vendor_package() { # <package dir> <files...>
  local pkg="$1"
  shift
  for f in "$@"; do
    copy_with_header "${PKG}/${pkg}/${f}" "${HERE}/vendor/${pkg}/${f}" \
      "packages/${pkg}/${f}" "${V1_LINE}"
  done
}
vendor_package saltcorn-data "${DATA_FILES[@]}"
vendor_package saltcorn-markup "${MARKUP_FILES[@]}"
vendor_package common-code "${COMMON_CODE_FILES[@]}"
vendor_package db-common "${DB_COMMON_FILES[@]}"
vendor_package saltcorn-types "${TYPES_FILES[@]}"
vendor_package plain-date "${PLAIN_DATE_FILES[@]}"

for entry in "${PUBLIC_FILES[@]}"; do
  src="${entry%%:*}"
  dest="${entry#*:}"
  copy_with_header "${PKG}/${src}" "${HERE}/public/${dest}" "packages/${src}" "${V1_LINE}"
done
# The icon font's binaries cannot carry a header; README lists where they came from.
mkdir -p "${HERE}/public/fontawesome-free"
cp -r "${PKG}/saltcorn-sbadmin2/public/fontawesome-free/webfonts" "${HERE}/public/fontawesome-free/"
cp "${PKG}/saltcorn-sbadmin2/public/fontawesome-free/LICENSE.txt" "${HERE}/public/fontawesome-free/"
# Plain Bootstrap 5.3 css: the monorepo only ships it compiled into sb-admin-2's
# theme, so it comes from Saltcorn's any-bootstrap-theme plugin (README).
copy_with_header "${THEME}/public/bootstrap.min.css" "${HERE}/public/bootstrap.min.css" \
  "saltcorn/any-bootstrap-theme public/bootstrap.min.css" \
  "@saltcorn/any-bootstrap-theme ${THEME_VERSION} (${THEME_COMMIT})"

# --- what a v1 `require` answers, for the bundle_shape test -------------------
# Run from the checkout so `@saltcorn/*` resolves to its workspace packages and
# the "require" export condition picks v1's CommonJS shims, exactly as a plugin's
# `require` would. Exits explicitly: loading @saltcorn/data opens handles.
(
  cd "${V1}"
  SPECS="${HERE}/src/library-specifiers.json" OUT="${HERE}/vendor/v1-exports.json" \
    VERSION_LINE="${V1_LINE}" node -e '
      const specs = require(process.env.SPECS);
      const out = { "//": "Generated by vendor/refresh.sh from " + process.env.VERSION_LINE + ": the export names a v1 require() answers with.", modules: {} };
      let failed = 0;
      for (const spec of specs) {
        try {
          const m = require(spec);
          out.modules[spec] = { type: typeof m, exports: Object.keys(m).filter((k) => k !== "__esModule").sort() };
        } catch (e) {
          failed++;
          console.error(`could not require ${spec} from the v1 checkout: ${e.message.split("\n")[0]}`);
        }
      }
      require("fs").writeFileSync(process.env.OUT, JSON.stringify(out, null, 2) + "\n");
      process.exit(failed ? 1 : 0);
    '
)

echo "vendored ${V1_LINE} into ${HERE}"
