#!/bin/sh
# Verification for the `style-done-tasks` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "style-done-tasks: $1"
  exit 1
}

grep -q 'line-through' src/app.css || fail "src/app.css has no line-through rule"
# `done` on its own is already all over the page (`task.done`), so what is
# looked for is a *class* chosen by it.
grep -qE 'task-done|line-through|className=\{[^}]*done' src/pages/Tasks.tsx ||
  fail "the list does not give a done task a class of its own"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "style-done-tasks: verified"
