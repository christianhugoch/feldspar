#!/bin/sh
# Verification for the `sort-the-list` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "sort-the-list: $1"
  exit 1
}

grep -qE '\.sort\(|toSorted\(' src/pages/Tasks.tsx src/lib/*.ts* || fail "nothing sorts the tasks"
grep -qE 'localeCompare|title <|title >' src/pages/Tasks.tsx src/lib/*.ts* || fail "the sort does not compare titles"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "sort-the-list: verified"
