#!/bin/sh
# Verification for the `add-a-page` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "add-a-page: $1"
  exit 1
}

test -f src/pages/About.tsx || fail "there is no src/pages/About.tsx"
grep -q '"/about"' src/routes.tsx || fail "/about is not routed in src/routes.tsx"
grep -q 'to="/about"' src/App.tsx || fail "nothing in the nav links to /about"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "add-a-page: verified"
