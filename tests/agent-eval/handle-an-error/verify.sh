#!/bin/sh
# Verification for the `handle-an-error` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "handle-an-error: $1"
  exit 1
}

grep -qE 'catch' src/pages/Tasks.tsx || fail "the failure is not caught"
grep -qE 'className="error"|class="error"' src/pages/Tasks.tsx || fail "the error is not shown in the error style"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "handle-an-error: verified"
