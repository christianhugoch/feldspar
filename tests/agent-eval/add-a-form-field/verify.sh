#!/bin/sh
# Verification for the `add-a-form-field` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "add-a-form-field: $1"
  exit 1
}

grep -q 'type="date"' src/pages/Tasks.tsx || fail "the form has no date input"
grep -qE 'due:\s*(due|dueDate|date)' src/pages/Tasks.tsx || fail "the new task is still created with a hard-coded due date"
grep -q 'due: null' src/pages/Tasks.tsx && fail "createTask still passes due: null"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "add-a-form-field: verified"
