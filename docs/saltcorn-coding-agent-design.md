# Design Recommendation: A Low-Cost Internal Coding Agent for Saltcorn

*Revision 2 (September 2026). All sources linked inline as numbered references; the full list is in [Sources](#sources) at the end.*

---

## TL;DR

- **Harness design is the cheapest performance lever.** A bash-only, ~100-line agent scores >74% on SWE-bench Verified [1](https://github.com/SWE-agent/mini-swe-agent); a small open-weight model (Qwen3.6-35B-A3B) reaches 57% with the same scaffold [2](https://github.com/SWE-bench/experiments/issues/447); and with a fixed model, swapping harnesses moved pass rates by 27 points and changed cost per task by more than 2x at equal quality [5](https://arxiv.org/abs/2606.12344) [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase).
- **Build a small, lean ReAct loop with ~7 tools**, a system prompt under ~1.5k tokens, and let the model edit files through tools while the harness produces the diff from git — never ask a weak model to write raw diff text [5](https://arxiv.org/abs/2606.12344).
- **Compensate for weak models with structure, not prompts**: a strong-model planner writes a machine-readable feature list once; a cheap executor does one feature per fresh-context session; every edit is auto-checked (format, types, lint, tests); a doom-loop detector and budget guard stop waste; stuck states escalate to a stronger model [24](https://docs.cline.bot/core-workflows/plan-and-act) [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents).
- **Budget context in absolute tokens, not percent of window.** Cheap models degrade well before their context limit; Aider observes most models get distracted above roughly 25k tokens [16](https://aider.chat/docs/troubleshooting/edit-errors.html).
- **Verification is platform-specific and CLI-first**: agent-browser for React [45](https://github.com/vercel-labs/agent-browser), agent-device (which has a typed Node.js API) for mobile [47](https://github.com/callstackincubator/agent-device), and a compile → host-test → emulator ladder (PlatformIO + QEMU/Renode, Wokwi CI) for embedded [52](https://docs.platformio.org/en/latest/advanced/unit-testing/simulators/index.html) [49](https://docs.wokwi.com/wokwi-ci/cli-usage).
- **Responses API**: construct requests statelessly (resend `instructions`, pass trimmed items), keep a byte-stable prefix for caching, and feature-detect capabilities because OpenAI-compatible servers differ [40](https://developers.openai.com/api/docs/guides/conversation-state) [41](https://developers.openai.com/api/docs/guides/migrate-to-responses).

---

## 1. Evidence base: what makes cheap models succeed

| Finding | Why it matters for this design | Source |
|---|---|---|
| mini-SWE-agent: only a `bash` tool, stateless `subprocess.run` per action, linear history; >74% on SWE-bench Verified. | A tiny scaffold is a viable baseline; complexity must earn its place. | [1](https://github.com/SWE-agent/mini-swe-agent) |
| Qwen3.6-35B-A3B + mini-swe-agent: 57.0% pass@1 on SWE-bench Verified (community submission, single attempt). | Cheap open-weight models are now in a useful range with a minimal harness. | [2](https://github.com/SWE-bench/experiments/issues/447) |
| Agentless (localize → repair → validate, no autonomous loop): 32% on SWE-bench Lite at $0.70 per issue. | Deterministic pipeline steps beat free-form agency for weak models on well-scoped fixes. | [3](https://arxiv.org/abs/2407.01489) |
| SWE-agent's Agent-Computer Interface: purpose-built file viewer, search commands and guardrails outperform a raw shell. | Tool ergonomics (line numbers, capped output, clear errors) matter. | [4](https://arxiv.org/abs/2405.15793) |
| Claw-SWE-Bench: same GLM 5.1 model, direct unified-diff output → 19.1%; edit-via-tools + runner-side git diff → 73.4%, apply failures under 1.5%. Across sweeps, model choice moved Pass@1 by 29.4 pp and harness choice by 27.4 pp. | Never make the model author patch text; export the diff from git. Harness ≈ as important as model. | [5](https://arxiv.org/abs/2606.12344) |
| Databricks internal benchmark: same model and effort through different harnesses → cost per task varied more than 2x with equal quality; medium/lower-intelligence models were effective on common tasks and much cheaper; simple harnesses like Pi often did best. | Lean context is a cost lever; cheap models are fine for routine work. | [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase) [7](https://earendil.com/posts/pi-autoresearch-and-databricks/) |
| Databricks also found agents recovering the reference solution from git history in the worktree. | Sandbox hygiene: agents exploit whatever is reachable. | [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase) |
| A test-iterating harness on SWE-bench Pro resolved 93.1% with a cheap open-weight model pair at about $0.41/instance (author notes the comparison is confounded by visible-test access). | A tight hypothesis → fix → run tests loop is extremely valuable when tests exist. | [8](https://github.com/kimjune01/swebench-pro) |
| Aider: unified-diff format raised GPT-4 Turbo from 20% to 61% on a refactoring benchmark and cut "lazy" elisions 3x. | Edit format choice is model-dependent and measurable. | [12](https://aider.chat/docs/unified-diffs.html) |
| Long context hurts even with perfect retrieval ("context rot"). | Keep windows small; compact early. | [11](https://research.trychroma.com/context-rot) |

---

## 2. Architecture overview

```
Saltcorn job / API request
        │
        ▼
┌──────────────────────────────────────────────────────────────┐
│ Task Orchestrator (outer loop, "sessions")                   │
│  • spec → planner (strong model) → feature_list.json         │
│  • for each feature: start FRESH context session             │
│  • after session: verify, git commit, update progress.md     │
│  • budgets: $ / turns / wall clock; escalation policy        │
└──────────────┬───────────────────────────────────────────────┘
               ▼
┌──────────────────────────────────────────────────────────────┐
│ Agent Loop (inner ReAct loop, cheap executor model)          │
│  Context Builder ─► Responses API ─► Tool Dispatcher         │
│      ▲                                   │                   │
│      │   Doom-loop guard / budget guard ◄┤                   │
│      └──── Tool results (pruned) ◄───────┘                   │
└──────────────┬───────────────────────────────────────────────┘
               ▼
┌──────────────────────────────────────────────────────────────┐
│ Tools: read · grep · glob · repo_map · edit · write · bash   │
│        · check (verification)                                │
│ Edit Engine (match cascade, formatter, diagnostics)          │
│ Verifier plugins: web / mobile / embedded                    │
│ Sandbox: Docker container + git worktree per task            │
│ Ledger: tokens, cached tokens, cost, trajectory log          │
└──────────────────────────────────────────────────────────────┘
```

Two nested loops is the key structural decision. The **outer loop** follows Anthropic's long-running harness pattern: an initializer step expands the spec into a structured feature list (JSON is harder for the model to corrupt than Markdown), and each subsequent session works on one feature, tests it end-to-end, writes a progress note and commits [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents). A fresh session per feature is the cheapest form of compaction, and it keeps a weak model inside the token range where it stays coherent [16](https://aider.chat/docs/troubleshooting/edit-errors.html). The **inner loop** is a plain ReAct tool loop, as minimal as mini-SWE-agent or Pi [1](https://github.com/SWE-agent/mini-swe-agent) [18](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/).

For bug-fix/feature tasks on existing code the orchestrator uses a shortened outer loop: localize → reproduce (write a failing test or check) → fix → validate, an agent-driven version of the Agentless pipeline [3](https://arxiv.org/abs/2407.01489).

---

## 3. Tool set

Keep the tool list small and non-overlapping. Anthropic's tool-design guidance: consolidate, namespace clearly, return meaningful context, and paginate/truncate with sensible defaults [35](https://www.anthropic.com/engineering/writing-tools-for-agents). Pi ships only read/write/edit/bash with a system prompt plus tool definitions under 1,000 tokens [18](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/). For *weaker* models I recommend a few more dedicated tools than Pi (grep/glob/check), because capped, structured outputs are safer than letting a cheap model compose shell pipelines that dump megabytes into context.

| Tool | Purpose | Implementation notes |
|---|---|---|
| `read` | Read a file or range. | Args `path`, `offset`, `limit`. Always prefix line numbers (the SWE-agent ACI lesson [4](https://arxiv.org/abs/2405.15793)). Default cap ~2,000 lines and per-line char cap; on truncation say exactly how to page. Detect binaries. Record a content hash so `edit` can detect stale reads. |
| `grep` | Text/regex search. | ripgrep-backed; respects `.gitignore`; returns `path:line: match` with a hard cap (e.g. 100 matches) and a "narrow your query" hint when capped. |
| `glob` | File discovery. | Pattern list; sorted by recency; capped. |
| `repo_map` | Structural overview ranked by relevance. | Port Aider's algorithm: tree-sitter tag extraction, file/symbol graph, PageRank personalised toward the files in play, binary search to fit a token budget (Aider defaults to 1k) [14](https://aider.chat/docs/repomap.html) [15](https://aider.chat/2023/10/22/repomap.html). Ports exist in Rust and Go to crib from [17](https://docs.rs/repo-mapper). In Node, use `web-tree-sitter` with vendored `tags.scm` queries. Cache per commit. |
| `edit` | Surgical change to an existing file. | See §3.1. |
| `write` | Create or fully overwrite a file. | Used heavily during initial build. Refuse overwrite of an existing file that has not been `read` this session (prevents blind clobbering). |
| `bash` | Builds, installs, git, arbitrary commands. | Timeout (default 120 s, max configurable); non-interactive env (`CI=1`, no pagers); output truncated to head + tail with the byte count elided; command-level dedup feeds the doom-loop guard. Long-running dev servers go through a managed helper (start/stop/logs) rather than background `&`. |
| `check` | Run the project's verification suite. | Runs the commands declared in AGENTS.md / project config (typecheck, lint, tests, build, platform verifier). Returns a **structured** summary: which check, pass/fail, first N errors with file:line, and whether each error is new vs pre-existing. The model does not need to remember commands. |

Deliberately **not** tools in the MVP:

- **Todo/plan tool.** Pi's documentation says built-in to-dos confuse models [19](https://www.npmjs.com/package/@mariozechner/pi-coding-agent); Anthropic's long-running harness instead uses a JSON feature list and a progress file [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents). Use files the orchestrator owns (see §5).
- **MCP servers for browser/device/simulator.** Drive them as CLIs via `bash`, with a short usage note in the platform prompt. CLI snapshots are far cheaper: one comparison measured about 5.5K characters of output for six browser tests with agent-browser versus about 31K with Playwright MCP [46](https://www.pulumi.com/blog/self-verifying-ai-agents-vercels-agent-browser-in-the-ralph-wiggum-loop/).

### 3.1 The edit engine (most important component)

1. **The model edits files; the harness makes the diff.** Final output of a task is `git diff` taken by the runner, never model-written patch text. This single choice took one harness from 19.1% to 73.4% with the same model [5](https://arxiv.org/abs/2606.12344) [5b](https://github.com/opensquilla/claw-swe-bench).
2. **Format per model family.**
   - *Default (all models):* `str_replace`-style `{path, old_text, new_text}` — the shape Pi notes models are already trained on [18](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/).
   - *OpenAI-family models:* offer the native `apply_patch` tool (V4A, context-anchored rather than line-number based) [43](https://developers.openai.com/api/docs/guides/tools-apply-patch) [28](https://github.com/openai/codex/blob/main/codex-rs/core/src/patch/v4a.md). OpenRouter exposes an equivalent server tool on its Responses endpoint and returns syntax errors to the model for self-correction [44](https://openrouter.ai/docs/guides/features/server-tools/apply-patch).
   - *Weakest models / small files:* fall back to whole-file `write`; Aider uses "whole" as its simplest format for weak models [13](https://aider.chat/docs/more/edit-formats.html).
   - Note that format preference varies by model and task; Diff-XYZ found no single best diff format across models [10](https://arxiv.org/html/2510.12487v2). Make the format a per-model config value and measure.
3. **Match cascade.** exact → whitespace/indent-normalised → trailing-whitespace/CRLF-insensitive → fuzzy (similarity threshold, unique best match only). Aider works hard to accept "almost correct" edits rather than rejecting them [16](https://aider.chat/docs/troubleshooting/edit-errors.html).
4. **Actionable failures.** If no unique match: return `status: failed` with the closest matching region (with line numbers) and a one-line instruction. The OpenAI apply_patch guide likewise recommends returning a failed status with a helpful message so the model can recover [43](https://developers.openai.com/api/docs/guides/tools-apply-patch).
5. **Post-edit feedback inline.** After a successful edit: run the formatter, then attach capped diagnostics (type/lint errors for the edited file plus a few other broken files). OpenCode's native edit/write/apply_patch path refreshes LSP state so diagnostics stay current [21](https://opencode.ai/docs/tools/) [22](https://github.com/anomalyco/opencode/issues/26118); magi-code documents the same "edit → instant diagnostics" loop with output caps and graceful degradation when the language server is unavailable [33](https://docs.rs/crate/magi-code/0.64.0/source/docs/features/lsp-diagnostics.md). Where running an LSP is too heavy, `tsc --noEmit` on changed files or the compiler is an acceptable substitute.
6. **Guards.** Path validation (no traversal outside the worktree), read-before-edit, stale-read detection via content hash, and all-or-nothing application for multi-file patches [43](https://developers.openai.com/api/docs/guides/tools-apply-patch).

---

## 4. System prompt

Target **≤1.5k tokens** for the core prompt plus tool definitions (Pi is under 1k [18](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)); Anthropic recommends writing at the "right altitude" — specific enough to guide, general enough not to be brittle — and organising with clear sections [34](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents). Everything in the prompt must be byte-stable within a session so it is cached.

Suggested skeleton (layered, most stable first):

```
<role>        You are a coding agent working in a sandboxed git worktree. </role>
<workflow>    1 Read AGENTS.md notes below and progress.md.
              2 Locate relevant code (repo_map, grep) before reading whole files.
              3 For bugs: reproduce first (a failing test or check).
              4 Make the smallest change; edit only files you have read.
              5 Run `check`. Fix until green. Do not stop at "should work".
              6 Finish with a 3–5 line summary of what changed and how verified. </workflow>
<rules>       Never delete, skip or weaken tests to make them pass.
              Never read git history for solutions / never modify CI config unless asked.
              Stay inside the task scope in feature_list.json. </rules>
<edit_format> (only the rules for the ACTIVE edit tool) </edit_format>
<platform>    (one of: react-web | mobile-expo | embedded-<toolchain>)
              commands, verifier usage, conventions </platform>
<project>     (AGENTS.md contents, nearest-file-wins) </project>
```

The test ratchet ("never remove or edit tests to make them pass") comes directly from Anthropic's long-running harness, where it prevents a common failure mode [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents) [36b](https://addyosmani.com/blog/long-running-agents/). The workflow block mirrors mini-SWE-agent's recommended sequence (find, reproduce, edit, re-run, check edge cases) [1](https://github.com/SWE-agent/mini-swe-agent). Avoid emotional or "tipping" prompts — Aider measured such folk remedies making results *worse* [12](https://aider.chat/docs/unified-diffs.html).

---

## 5. Planning

**Split planning from execution and use models accordingly.**

- **Spec → plan (strong model, once).** Produce `feature_list.json`: ordered features, each with acceptance criteria, files likely touched, and the verification command that proves it. This is the initializer role in Anthropic's harness [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents). It costs a few strong-model calls and saves many cheap-model wanderings.
- **Execution (cheap model, per feature).** One feature per fresh session; the session reads `progress.md`, `feature_list.json` and recent `git log`, implements, runs `check`, commits, updates progress, and stops.
- **Plan/Act separation with different models** is a proven pattern: Cline lets you assign a stronger reasoning model to its read-only Plan mode and a faster one to Act mode [24](https://docs.cline.bot/core-workflows/plan-and-act); it exposes a mode-switch tool only in plan mode [25](https://deepwiki.com/cline/cline/3.4-plan-and-act-modes). Aider's architect/editor split is the same idea [13](https://aider.chat/docs/more/edit-formats.html).
- **Review loop (optional, v1).** A proposal in the Cline community describes the planner reviewing the actual git diff and test output after each Act step rather than trusting the executor's description, with a cap on review cycles [26](https://github.com/cline/cline/discussions/12959). This is a good use of a strong model: short input (diff + test summary), high leverage.
- **Re-plan triggers.** Two consecutive failed `check` cycles on the same feature, or a doom-loop warning, sends the feature back to the planner with the failure summary.

---

## 6. Context management and compaction

1. **Absolute working budget.** Configure a per-model working budget (e.g. 24–40k tokens for cheap models) regardless of the advertised window [16](https://aider.chat/docs/troubleshooting/edit-errors.html) [11](https://research.trychroma.com/context-rot). Trigger compaction at ~75% of that budget.
2. **Session boundaries first.** Most "compaction" should happen by ending a session at a feature boundary and starting fresh with the progress file — Anthropic's harness does this deliberately [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents).
3. **Tool-result clearing before summarisation.** Replace stale tool outputs with stubs (e.g. `[elided: 412 lines of npm test output — 2 failures: …]`), keep only the latest read of each file. Anthropic's cookbook shows file-read results dominating context in long runs and demonstrates clearing plus compaction plus memory as complementary primitives [37](https://platform.claude.com/cookbook/tool-use-context-engineering-context-engineering-tools).
4. **Structured compaction prompt.** Summarise into fixed sections: goal, decisions made, files changed, current failing checks, next step. Production thresholds for reference: Claude Code compacts around 80%+ of window and Codex CLI around 90% [30](https://gist.github.com/badlogic/cd2ef65b0697c4dbe2d13fbecb0a0a5f); for cheap models trigger earlier.
5. **Just-in-time retrieval.** Keep paths and symbol names in context, load content on demand [34](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents).
6. **Cache-friendly layout.** Stable prefix (system prompt, tool schemas, platform notes, AGENTS.md) → session header (repo map, feature spec; fixed for the session) → append-only turn history. Never reorder or edit earlier items between compactions. Prompt caching can cut input costs by up to 90% on cached prefixes [42](https://developers.openai.com/cookbook/examples/prompt_caching_201).
7. **Subagents only for context isolation.** An `explore` subagent (read-only tools, cheapest model) that returns a short brief is the one subagent worth having; Claude Code's built-in read-only exploration agent exists for the same reason — keeping search output out of the main context [29](https://code.claude.com/docs/en/subagents-and-plugins.md). Defer to v2.

---

## 7. Memory

| Layer | What | Written by |
|---|---|---|
| `AGENTS.md` | Build/test/lint commands, conventions, gotchas. Nearest file in the directory tree takes precedence; plain Markdown, no schema [38](https://agents.md/). Now stewarded under the Linux Foundation's Agentic AI Foundation [38](https://agents.md/). | Humans (agent may propose edits) |
| `feature_list.json` | Plan + per-feature status and acceptance criteria. | Planner; executor updates status only |
| `progress.md` | Rolling handoff note for the next session. | Executor, each session |
| `git log` | Durable record of what changed and why. | Harness commits with agent-written messages |
| `.agent/notes.md` (optional) | Learned project facts (e.g. "tests need `TZ=UTC`"). Keep short; human-reviewable. | Executor, sparingly |

Plain files in the repo are what the model already knows how to read and write; Anthropic's harness relies on exactly a JSON feature list, a progress file and git history [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents). No vector store is needed initially.

---

## 8. Code search

Order of investment for cheap models:

1. `grep` + `glob` with capped outputs (always).
2. `repo_map` (tree-sitter + PageRank) — the highest-leverage addition; it compresses a codebase into a ranked symbol overview within a fixed budget [14](https://aider.chat/docs/repomap.html) [15](https://aider.chat/2023/10/22/repomap.html).
3. LSP queries (definition/references) — OpenCode offers these as an experimental opt-in tool [21](https://opencode.ai/docs/tools/); note its V2 rewrite does not yet ship the LSP runtime [23](https://opencode.ai/v2/docs/lsp/), a hint that LSP integration is costly to maintain. Use LSP for diagnostics first, navigation later.
4. Embeddings / semantic search — only if large-repo localisation fails in your benchmarks. Agentless's hierarchical localisation (files → classes/functions → lines) is a cheaper first step [3](https://arxiv.org/abs/2407.01489).

---

## 9. Verification and testing

**General rules**

- `check` runs deterministic checks; do not trust an LLM judge for pass/fail — Databricks avoided LLM judging because it rewards sounding right over being right [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase).
- Feed back structured, specific failures (check, file:line, message, new vs pre-existing).
- For bugs: require a reproducing check before the fix; for features: acceptance criteria in `feature_list.json` map to concrete commands.
- Test ratchet in the prompt and in code: the harness can diff the test directory and flag deleted/skipped tests [36](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents).

**Per platform**

| Platform | Cheap gate (every edit) | Behavioural gate (per feature) | Tools |
|---|---|---|---|
| React web | `tsc --noEmit`, ESLint, Vitest on affected files | Build, start dev server, drive the app via accessibility snapshot + refs; screenshot as fallback | agent-browser: snapshots with compact `@eN` refs, click/fill/screenshot, optional MCP [45](https://github.com/vercel-labs/agent-browser); snapshot workflow uses ~200–400 tokens per page [45b](https://github.com/vercel-labs/agent-browser/blob/main/skill-data/core/SKILL.md) |
| Mobile (Expo / React Native) | `tsc`, Jest, `expo export` | Run on emulator/simulator; snapshot, tap, screenshot, logs; save replay scripts for regression | agent-device: CLI, MCP and typed Node.js API across iOS/Android simulators, emulators and devices; React Native/Expo component inspection; replayable `.ad` scripts and Maestro flows [47](https://github.com/callstackincubator/agent-device) [47b](https://oss.callstack.com/agent-device/docs/introduction) [48](https://docs.expo.dev/agents/agent-device/) |
| Embedded | Cross-compile (`pio run`, `idf.py build`, `west build`), `-Werror` | (1) Host unit tests (Unity/Ceedling [53](https://www.throwtheswitch.org/ceedling)); (2) emulator tests; (3) cloud sim with serial assertions | PlatformIO `test_testing_command` plugs in QEMU, Renode or SimAVR with `pio test --without-uploading` [52](https://docs.platformio.org/en/latest/advanced/unit-testing/simulators/index.html); Renode emulates UART/SPI/I2C/GPIO etc. and integrates with Robot Framework and a GitHub Action [50](https://github.com/renode/renode) [50b](https://github.com/antmicro/renode-test-action) [51](https://interrupt.memfault.com/blog/test-automation-renode); Wokwi CLI with `--expect-text`/`--fail-text` [49](https://docs.wokwi.com/wokwi-ci/cli-usage), `idf.py wokwi` for ESP-IDF [49b](https://docs.wokwi.com/wokwi-ci/idf-wokwi-usage), experimental MCP server [49c](https://docs.wokwi.com/wokwi-ci/mcp-support) |

For Saltcorn specifically, agent-device's typed Node client [47](https://github.com/callstackincubator/agent-device) means the mobile verifier can be a normal Node module rather than a subprocess wrapper.

---

## 10. Loop control and budgets

- **Doom-loop detection.** Fingerprint `(tool, canonicalised args)` — canonicalise JSON (key order, whitespace) or semantically identical repeats are missed [32b](https://github.com/OpenRouterTeam/typescript-agent/pull/73). Kilo Code pauses after 3 identical consecutive calls [31](https://github.com/NousResearch/hermes-agent/issues/512). Also detect repeated *fan-outs* of distinct calls reissued each round, and repeated assistant text; OpenRouter's TypeScript agent SDK implements all three and lets each tool declare what counts as "the same call" [32](https://openrouter.ai/docs/agent-sdk/call-model/doom-loop-detection) [32c](https://github.com/OpenRouterTeam/typescript-agent/pull/89). Expect cheap models to trigger it more often [20](https://pi.dev/packages/pi-anti-doom-loop).
- **Escalation ladder.** warn (inject guidance) → skip execution and force re-plan → switch to stronger model for one step → abort, commit work-in-progress to a branch, record trajectory. Pi's anti-doom-loop package uses a similar steer → abort-and-resume-once → hand back ladder with a capped resume budget [20](https://pi.dev/packages/pi-anti-doom-loop).
- **Format-error cap.** Cap consecutive malformed tool calls (mini-SWE-agent added exactly this) [1b](https://github.com/SWE-agent/mini-swe-agent/releases).
- **Budgets.** Max turns per session, max sessions per feature, $ ceiling per task (from usage fields, including cached tokens), wall-clock limit.
- **Sandbox hygiene.** Fresh container + worktree per task; strip or hide history that could leak solutions where relevant [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase); network allowlist for package registries only.

---

## 11. Model routing and cost

| Role | Model tier | Notes |
|---|---|---|
| Planner (spec → feature list; re-plan) | Strong / high reasoning | Few calls, high leverage [24](https://docs.cline.bot/core-workflows/plan-and-act) |
| Executor (edit/test loop) | Cheapest model that passes your benchmark slice | Bulk of tokens; Databricks found medium/lower tiers effective on common tasks [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase) |
| Reviewer (diff + test summary) | Strong, optional | Short inputs [26](https://github.com/cline/cline/discussions/12959) |
| Summariser / commit messages / explore subagent | Cheapest | Aider uses a "weak model" for such side tasks [16](https://aider.chat/docs/troubleshooting/edit-errors.html) |

Escalate executor → strong for a single step on: repeated `check` failure, doom-loop warning, or edit-match failure after the cascade. Log per-role cost so routing thresholds can be tuned from data.

---

## 12. Responses API and Node/TypeScript notes

- **State strategy: stateless by default.** Chaining with `previous_response_id` still bills all prior input tokens, and it does not carry the previous top-level `instructions` — resend them every request [40](https://developers.openai.com/api/docs/guides/conversation-state) [41](https://developers.openai.com/api/docs/guides/migrate-to-responses). Since this agent prunes and compacts its own history, pass trimmed input items yourself.
- **Reasoning models.** Responses persists reasoning across turns; OpenAI reports 40–80% better cache utilisation versus Chat Completions for this reason [39](https://developers.openai.com/blog/responses-api). When stateless, use `store: false` and replay the encrypted reasoning items [41](https://developers.openai.com/api/docs/guides/migrate-to-responses) [42](https://developers.openai.com/cookbook/examples/prompt_caching_201).
- **Server-side compaction.** OpenAI documents a standalone `/responses/compact` endpoint [40](https://developers.openai.com/api/docs/guides/conversation-state); Azure's Responses implementation does not expose it [40b](https://learn.microsoft.com/en-au/answers/questions/5791830/azure-openai-response-api-input-token-counting-wit). Implement compaction in the harness and treat server features as optional accelerators.
- **Capability detection.** "OpenAI-compatible" servers vary (native `apply_patch`, reasoning items, compaction, parallel tool calls). Keep a per-provider capability table and fall back to function tools + harness-side compaction.
- **Tool results for apply_patch.** Return `apply_patch_call_output` with `status` and a useful message on failure [43](https://developers.openai.com/api/docs/guides/tools-apply-patch).
- **Parallel tool calls.** Disable for weak models in the MVP; sequential calls are easier to fingerprint and debug.
- **Integration.** Build the loop in TypeScript on top of the existing `@saltcorn/large-language-model` plugin's provider layer (Vercel AI SDK), with the agent loop, tool registry, edit engine and verifiers as Saltcorn-side modules; run the sandbox in Docker. Pi's `pi-ai`/agent-runtime packages and DeepSeek Harness (Node, "everything is a plugin" on the Cordis framework) are useful TypeScript/Node references for structure [19](https://www.npmjs.com/package/@mariozechner/pi-coding-agent) [27](https://github.com/deepseek-ai/deepseek-harness) — note DeepSeek Harness is a developer preview with announced breaking changes [27](https://github.com/deepseek-ai/deepseek-harness).

---

## 13. How the surveyed agents compare

| Agent | Core tools | Edit mechanism | Planning | Context / compaction | Memory | Takeaway for this design |
|---|---|---|---|---|---|---|
| **mini-SWE-agent** [1](https://github.com/SWE-agent/mini-swe-agent) | bash only | Shell (sed, heredocs, etc.) | Workflow in prompt | Linear history | — | Proves the minimal baseline |
| **Pi** [18](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/) [19](https://www.npmjs.com/package/@mariozechner/pi-coding-agent) | read, write, edit, bash | exact-match edit | None built in (plans as files) | Lean prompt (<1k tokens), ~3x less context per turn in Databricks' test [7](https://earendil.com/posts/pi-autoresearch-and-databricks/) | Files / extensions | Context discipline = cost advantage |
| **OpenCode** [21](https://opencode.ai/docs/tools/) | read, grep, glob, edit, write, bash, apply_patch, task, experimental LSP | edit / apply_patch with LSP refresh | Agents / modes | Provider-specific transforms | AGENTS.md | Post-edit diagnostics loop |
| **Cline** [24](https://docs.cline.bot/core-workflows/plan-and-act) [25](https://deepwiki.com/cline/cline/3.4-plan-and-act-modes) | file, search, terminal, browser | diff blocks | Read-only Plan mode, separate Plan/Act models | Mode notices on transition | Rules files | Plan/Act model split |
| **Codex CLI** [28](https://github.com/openai/codex/blob/main/codex-rs/core/src/patch/v4a.md) | shell, apply_patch | V4A patches | Plan tool | Auto-compaction near window limit [30](https://gist.github.com/badlogic/cd2ef65b0697c4dbe2d13fbecb0a0a5f) | AGENTS.md | Use native apply_patch for OpenAI models |
| **Claude Code** [29](https://code.claude.com/docs/en/subagents-and-plugins.md) | read, edit, write, grep, glob, bash, subagents | str_replace | Plan mode + Plan subagent | Auto-compaction [30](https://gist.github.com/badlogic/cd2ef65b0697c4dbe2d13fbecb0a0a5f); subagents for isolation | CLAUDE.md, subagent memory | Subagents only for context isolation |
| **Aider** [13](https://aider.chat/docs/more/edit-formats.html) [14](https://aider.chat/docs/repomap.html) | (non-tool, edit blocks) | whole / search-replace / udiff / patch per model | Architect/editor | Repo map with token budget | Conventions file | Repo map; per-model edit format |
| **DeepSeek Harness** [27](https://github.com/deepseek-ai/deepseek-harness) | Plugin-defined | Plugin-defined | Plugin-defined | Plugin-defined | Plugin memory (community) | Plugin micro-kernel in Node; preview-stage |

---

## 14. Roadmap

**MVP (weeks 1–3)**
Inner ReAct loop over Responses API; tools `read`, `grep`, `glob`, `edit` (str_replace + cascade; apply_patch for OpenAI models), `write`, `bash`, `check`; runner-side git diff; ≤1.5k-token cached system prompt + AGENTS.md; auto format + typecheck after edits; turn/$ budgets; basic doom-loop guard. Build a benchmark slice of ~30 real Saltcorn-style tasks (React, Expo, embedded) plus a SWE-bench Verified subset run with your cheap model.

**v1 (weeks 4–8)**
Outer session loop with planner → `feature_list.json` → per-feature fresh sessions + `progress.md`; `repo_map`; tool-result clearing and structured compaction on an absolute budget; model escalation ladder; React verifier with agent-browser; reviewer step on diffs.

**v2**
Mobile verifier (agent-device Node API); embedded ladder (PlatformIO + QEMU/Renode, Wokwi CI); `explore` subagent; learned-notes memory; LSP navigation; per-role cost dashboards.

**Decision triggers from benchmarks**
- Edit failures high → change the per-model edit format (apply_patch / whole-file) before changing the model [13](https://aider.chat/docs/more/edit-formats.html).
- Localisation failures high → invest in repo map and Agentless-style localisation [3](https://arxiv.org/abs/2407.01489).
- Input tokens dominate cost → tighten clearing/compaction, verify cache hit rate from usage fields [42](https://developers.openai.com/cookbook/examples/prompt_caching_201).
- Cheap executor within ~10 points of strong model on your slice → keep it; otherwise raise executor tier or add the reviewer step.

---

## 15. Caveats

- SWE-bench-style numbers measure Python issue resolution; transfer to React/mobile/embedded client code is plausible but unproven — your own benchmark slice is the real test.
- Several figures are vendor- or community-reported: the Pi/Databricks framing [7](https://earendil.com/posts/pi-autoresearch-and-databricks/) (the Databricks post itself is primary [6](https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase)), the Qwen submission [2](https://github.com/SWE-bench/experiments/issues/447), and the SWE-bench Pro harness result, which its author flags as confounded [8](https://github.com/kimjune01/swebench-pro).
- Compaction thresholds for Claude Code and Codex come from a community research gist [30](https://gist.github.com/badlogic/cd2ef65b0697c4dbe2d13fbecb0a0a5f) and may have changed.
- Wokwi's MCP server is explicitly experimental [49c](https://docs.wokwi.com/wokwi-ci/mcp-support); emulators and simulators do not replace on-hardware testing (Espressif's own Wokwi page notes simulation can differ from hardware [49d](https://docs.espressif.com/projects/esp-idf/zh_CN/latest/esp32/third-party-tools/wokwi.html)).
- DeepSeek Harness is a developer preview with breaking changes expected [27](https://github.com/deepseek-ai/deepseek-harness); OpenCode V2 does not yet include its LSP runtime [23](https://opencode.ai/v2/docs/lsp/).
- The field moves monthly; re-run the benchmark slice whenever you change models or providers.

---

## Sources

moved to the file docs/saltcorn-coding-agent-design-sources.md