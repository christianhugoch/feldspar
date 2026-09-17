# The seed eval suite

Ten tasks on a React + Vite project, for `feldspar agent eval` (TODO §13).

```sh
./make-fixture.sh                     # once, and after changing the fixture
feldspar agent eval tests/agent-eval \
  --model anthropic/claude-sonnet-4-5 \
  --strong anthropic/claude-opus-4-1 \
  --environment test
```

It writes `report.json` and `report.md` here. `docs/AGENT_EVAL.md` says what the
numbers mean and what to change when they are bad.

## What is here

- `fixture/` — the project every task starts from: the layout a scaffolded
  Feldspar `react` application has (`src/pages/`, `src/routes.tsx`,
  `src/feldspar/client.ts`, an `AGENTS.md` naming the checks), small enough to
  read. Its baseline is green: `npm run typecheck` and `npm test` both pass
  before any task starts, so nothing is scored on a failure it inherited.
- `make-fixture.sh` — `npm install` for the fixture, installed *beside* it and
  reached through a symlink so all ten task copies share one `node_modules`.
- One directory per task, each with a `task.toml` and a `verify.sh`.

## The tasks

| Task | What it asks for |
|---|---|
| `add-a-page` | A new page, routed and linked — the smallest whole change. |
| `add-a-form-field` | A field added to a form and carried through to the API call. |
| `add-a-list-filter` | State added to a list and used to filter it. |
| `fix-a-bug-with-a-test` | A reproducing test **first**, then the fix (TODO 9.7's ratchet). |
| `extract-a-hook` | A refactor across two files with no behaviour change. |
| `sort-the-list` | Ordering by two keys — easy to half-do. |
| `show-an-empty-state` | A conditional branch that is easy to get inverted. |
| `handle-an-error` | The failure path, which a model asked for the happy path skips. |
| `style-done-tasks` | A change spanning CSS and the component that names it. |
| `delete-a-task` | A mutation plus the local state that has to follow it. |

## How a task is scored

`verify.sh`'s exit status, and nothing else. Each one greps for the change and
then runs the project's own checks (`npm run typecheck`, `npm test`): the greps
judge whether the change is *there*, the checks judge whether it *works*. A
model that says it has finished is not believed — that is the harness's own
self-test (`crates/sc-cli/tests/agent_eval.rs`).

Every script is written to **fail on the untouched fixture**, which is worth
re-checking after editing one:

```sh
for t in */verify.sh; do (cd fixture && sh "../$t" >/dev/null 2>&1) &&
  echo "PASSES UNCHANGED: $t"; done
```

## Deviation: the tasks are `code` applications

The fixture is a React project, but each task's application uses the `code`
framework rather than `react`. A `react` application regenerates
`src/feldspar/` from **its own** tables and endpoints on every build, and the
eval's application is created over a copied directory with no tables — so a
`react` build would replace the fixture's client with an empty one and fail
every type check for a reason that has nothing to do with the model. The `code`
framework builds the project with `npm run build` and leaves the source alone,
which is what is wanted: what is being measured is the agent on a React tree,
not the scaffolder.

The one thing this loses is the `react` builder prompt's paragraph about
`src/feldspar/`; the fixture's `AGENTS.md` says the same thing, which is where a
`code` application's conventions belong anyway.
