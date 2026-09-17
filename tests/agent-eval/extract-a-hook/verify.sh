#!/bin/sh
# Verification for the `extract-a-hook` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "extract-a-hook: $1"
  exit 1
}

test -f src/lib/useTasks.ts || fail "there is no src/lib/useTasks.ts"
grep -q 'useTasks' src/pages/Tasks.tsx || fail "the tasks page does not use the hook"
grep -q 'useTasks' src/pages/Done.tsx || fail "the done page does not use the hook"
grep -q 'listTasks' src/pages/Done.tsx && fail "the done page still fetches for itself"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "extract-a-hook: verified"
