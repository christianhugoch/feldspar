#!/bin/sh
# Verification for the `delete-a-task` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "delete-a-task: $1"
  exit 1
}

grep -q 'deleteTask' src/pages/Tasks.tsx || fail "nothing calls deleteTask"
grep -qiE '>\s*Delete|aria-label="Delete' src/pages/Tasks.tsx || fail "there is no Delete button"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "delete-a-task: verified"
