#!/bin/sh
# Verification for the `add-a-list-filter` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "add-a-list-filter: $1"
  exit 1
}

grep -qi 'hide done' src/pages/Tasks.tsx || fail "there is no Hide done control"
grep -qE 'filter\(' src/pages/Tasks.tsx || fail "the list is not filtered"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "add-a-list-filter: verified"
