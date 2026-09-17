#!/bin/sh
# Verification for the `show-an-empty-state` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "show-an-empty-state: $1"
  exit 1
}

grep -q 'Nothing to do' src/pages/Tasks.tsx || fail "there is no empty state"
grep -qE 'length === 0|length < 1|!tasks\.length' src/pages/Tasks.tsx || fail "the empty state is not conditional on an empty list"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "show-an-empty-state: verified"
