#!/bin/sh
# Verification for the `fix-a-bug-with-a-test` task: run in the agent's copy of the fixture.
#
# Structural first, then the project's own checks. The greps cannot judge
# whether the change is *good* — they judge whether it is there — and the type
# check and the tests judge whether it works.
set -u

fail() {
  echo "fix-a-bug-with-a-test: $1"
  exit 1
}

grep -q 'tomorrow' src/lib/due.test.ts || fail "no test covers the tomorrow case"
grep -q 'tomorrow' src/lib/due.ts || fail "formatDue still cannot say tomorrow"
node --input-type=module -e "
  const { formatDue } = await import('./src/lib/due.ts');
  const now = new Date('2026-01-01T00:00:00Z');
  if (formatDue('2026-01-02T00:00:00Z', now) !== 'tomorrow') {
    console.error('formatDue still says ' + formatDue('2026-01-02T00:00:00Z', now));
    process.exit(1);
  }
" || fail "formatDue does not say tomorrow for a task due tomorrow"

npm run --silent typecheck || fail "the project does not type-check"
npm test --silent || fail "the tests do not pass"
echo "fix-a-bug-with-a-test: verified"
