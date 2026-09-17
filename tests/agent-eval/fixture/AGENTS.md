# The tasks application

A React + Vite project in TypeScript, in the shape a Feldspar `react`
application is scaffolded in.

## The checks

Run these before saying a change is finished:

1. `npm run typecheck` — `tsc --noEmit` over the whole project.
2. `npm test` — the unit tests (`vitest run`).
3. `npm run build` — the type check and then the bundle.

## Conventions

- `src/feldspar/` is the client generated from the application's API and is
  rewritten on every build: read it to learn what data there is, never edit it,
  and reach data only through it.
- A page is a component in `src/pages/`, exported by name and routed from
  `src/routes.tsx`.
- Styling is plain CSS in `src/app.css`, addressed by class name.
