# Design Recommendation: A Low-Cost Internal Coding Agent for Saltcorn

## Sources

### Research and benchmarks
1. mini-SWE-agent (README) — https://github.com/SWE-agent/mini-swe-agent
   1b. mini-SWE-agent releases (consecutive format-error cap) — https://github.com/SWE-agent/mini-swe-agent/releases
2. SWE-bench experiments issue #447: Qwen3.6-35B-A3B + mini-swe-agent, 57.0% on Verified — https://github.com/SWE-bench/experiments/issues/447
3. Xia et al., *Agentless: Demystifying LLM-based Software Engineering Agents* (arXiv:2407.01489) — https://arxiv.org/abs/2407.01489
4. Yang et al., *SWE-agent: Agent-Computer Interfaces Enable Automated Software Engineering* (arXiv:2405.15793) — https://arxiv.org/abs/2405.15793
5. *Claw-SWE-Bench: A Benchmark for Evaluating OpenClaw-style Agent Harnesses on Coding Tasks* (arXiv:2606.12344) — https://arxiv.org/abs/2606.12344
   5b. Claw-SWE-Bench code (runner-side patch collection) — https://github.com/opensquilla/claw-swe-bench
6. Databricks, *Benchmarking Coding Agents on Databricks' Multi-Million Line Codebase* — https://www.databricks.com/blog/benchmarking-coding-agents-databricks-multi-million-line-codebase
7. Earendil, *Pi, Minimal and Performant* — https://earendil.com/posts/pi-autoresearch-and-databricks/
8. kimjune01/swebench-pro (test-iterating harness; confounded comparison noted by author) — https://github.com/kimjune01/swebench-pro
9. *Building Effective AI Coding Agents for the Terminal: Scaffolding, Harness, Context Engineering, and Lessons Learned* (arXiv:2603.05344) — https://arxiv.org/pdf/2603.05344
10. *Diff-XYZ: A Benchmark for Evaluating Diff Understanding* (arXiv:2510.12487) — https://arxiv.org/html/2510.12487v2
11. Chroma, *Context Rot: How Increasing Input Tokens Impacts LLM Performance* — https://research.trychroma.com/context-rot

### Aider
12. *Unified diffs make GPT-4 Turbo 3X less lazy* — https://aider.chat/docs/unified-diffs.html
13. Edit formats — https://aider.chat/docs/more/edit-formats.html
14. Repository map — https://aider.chat/docs/repomap.html
15. *Building a better repository map with tree sitter* — https://aider.chat/2023/10/22/repomap.html
16. File editing problems (troubleshooting) — https://aider.chat/docs/troubleshooting/edit-errors.html
17. repo-mapper (Rust port of Aider's repo map) — https://docs.rs/repo-mapper

### Open-source agents
18. Mario Zechner, *What I learned building an opinionated and minimal coding agent* — https://mariozechner.at/posts/2025-11-30-pi-coding-agent/
19. `@mariozechner/pi-coding-agent` (npm README) — https://www.npmjs.com/package/@mariozechner/pi-coding-agent
20. pi-anti-doom-loop package — https://pi.dev/packages/pi-anti-doom-loop
21. OpenCode tools documentation — https://opencode.ai/docs/tools/
22. OpenCode issue #26118 (native tools refresh LSP state after edits) — https://github.com/anomalyco/opencode/issues/26118
23. OpenCode V2 LSP documentation — https://opencode.ai/v2/docs/lsp/
24. Cline, Plan & Act mode — https://docs.cline.bot/core-workflows/plan-and-act
25. DeepWiki, Cline Plan and Act modes (implementation) — https://deepwiki.com/cline/cline/3.4-plan-and-act-modes
26. Cline discussion #12959, automatic Plan↔Act review loop — https://github.com/cline/cline/discussions/12959
27. DeepSeek Harness — https://github.com/deepseek-ai/deepseek-harness (docs: https://deepseek-harness.github.io/deepseek-harness/)
28. OpenAI Codex, V4A patch format spec — https://github.com/openai/codex/blob/main/codex-rs/core/src/patch/v4a.md
29. Claude Code, Create custom subagents — https://code.claude.com/docs/en/subagents-and-plugins.md
30. Context compaction research: Claude Code, Codex CLI, OpenCode, Amp (gist) — https://gist.github.com/badlogic/cd2ef65b0697c4dbe2d13fbecb0a0a5f
31. Hermes Agent issue #512 (describes Kilo Code's doom-loop detection) — https://github.com/NousResearch/hermes-agent/issues/512
32. OpenRouter Agent SDK, doom-loop detection — https://openrouter.ai/docs/agent-sdk/call-model/doom-loop-detection
    32b. OpenRouter typescript-agent PR #73 (doom-loop detector, JSON canonicalisation) — https://github.com/OpenRouterTeam/typescript-agent/pull/73
    32c. OpenRouter typescript-agent PR #89 (fan-out repeat detection) — https://github.com/OpenRouterTeam/typescript-agent/pull/89
33. magi-code, LSP diagnostics after edits — https://docs.rs/crate/magi-code/0.64.0/source/docs/features/lsp-diagnostics.md

### Anthropic, OpenAI and standards
34. Anthropic, *Effective context engineering for AI agents* — https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
35. Anthropic, *Writing effective tools for agents — with agents* — https://www.anthropic.com/engineering/writing-tools-for-agents
36. Anthropic, *Effective harnesses for long-running agents* — https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents
    36b. Addy Osmani, *Long-running Agents* (summary of the above, incl. test ratchet) — https://addyosmani.com/blog/long-running-agents/
37. Claude Cookbook, *Context engineering: memory, compaction, and tool clearing* — https://platform.claude.com/cookbook/tool-use-context-engineering-context-engineering-tools
38. AGENTS.md — https://agents.md/ (repo: https://github.com/agentsmd/agents.md)
39. OpenAI, *Why we built the Responses API* — https://developers.openai.com/blog/responses-api
40. OpenAI, Conversation state — https://developers.openai.com/api/docs/guides/conversation-state
    40b. Microsoft Q&A, Azure Responses API token counting and compaction support — https://learn.microsoft.com/en-au/answers/questions/5791830/azure-openai-response-api-input-token-counting-wit
41. OpenAI, Migrate to the Responses API — https://developers.openai.com/api/docs/guides/migrate-to-responses
42. OpenAI Cookbook, *Prompt Caching 201* — https://developers.openai.com/cookbook/examples/prompt_caching_201
43. OpenAI, Apply Patch tool guide — https://developers.openai.com/api/docs/guides/tools-apply-patch
44. OpenRouter, Apply Patch server tool — https://openrouter.ai/docs/guides/features/server-tools/apply-patch

### Verification tooling
45. Vercel Labs, agent-browser — https://github.com/vercel-labs/agent-browser
    45b. agent-browser core skill (snapshot/ref workflow, token footprint) — https://github.com/vercel-labs/agent-browser/blob/main/skill-data/core/SKILL.md
46. Pulumi, *Self-Verifying AI Agents: Vercel's Agent-Browser in the Ralph Wiggum Loop* — https://www.pulumi.com/blog/self-verifying-ai-agents-vercels-agent-browser-in-the-ralph-wiggum-loop/
47. Callstack, agent-device — https://github.com/callstackincubator/agent-device
    47b. agent-device documentation — https://oss.callstack.com/agent-device/docs/introduction
48. Expo, agent-device and Expo — https://docs.expo.dev/agents/agent-device/
49. Wokwi CLI usage — https://docs.wokwi.com/wokwi-ci/cli-usage
    49b. Wokwi `idf.py wokwi` usage — https://docs.wokwi.com/wokwi-ci/idf-wokwi-usage
    49c. Wokwi MCP support — https://docs.wokwi.com/wokwi-ci/mcp-support
    49d. Espressif ESP-IDF, Wokwi third-party tools page — https://docs.espressif.com/projects/esp-idf/zh_CN/latest/esp32/third-party-tools/wokwi.html
    (Wokwi CI getting started — https://docs.wokwi.com/wokwi-ci/getting-started)
50. Renode — https://github.com/renode/renode
    50b. Antmicro renode-test-action — https://github.com/antmicro/renode-test-action
51. Interrupt (Memfault), *Firmware Testing with Renode and GitHub Actions* — https://interrupt.memfault.com/blog/test-automation-renode
52. PlatformIO, Unit testing with simulators (QEMU, Renode, SimAVR) — https://docs.platformio.org/en/latest/advanced/unit-testing/simulators/index.html
53. ThrowTheSwitch, Ceedling — https://www.throwtheswitch.org/ceedling
