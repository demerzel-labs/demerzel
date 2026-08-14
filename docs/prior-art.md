# Prior art

Demerzel builds on ideas from coding agents that came before it. This
page names each source. Demerzel reimplements these ideas from their
public descriptions and, where the license permits, from a reading of
their source code. Demerzel does not copy code from other projects.

| Project | License | What Demerzel learned |
|---|---|---|
| [Codex CLI](https://github.com/openai/codex) | Apache-2.0 | The wire contract for the ChatGPT Codex endpoint and for the PKCE login flow. The source of imported credentials. The `openai-codex` adapter and the `oauth` module name it for this reason. |
| [oh-my-pi](https://github.com/can1357/oh-my-pi) | MIT | The snapshot-tag edit scheme behind the `read` and `edit` tools. The competitor-config auto-discovery wedge. Reimplemented in Rust from a reading of the TypeScript source. |
| [jcode](https://github.com/1jehuang/jcode) | MIT | The performance bar: boot time and PSS as product features. The memory-system feature set. Benchmark reports cite its published numbers as the comparison target. |
| Pi | — | The small-defaults philosophy: a small tool set, a small prompt, opt-in capabilities. The fully replaceable system prompt. |
| Claude Code | proprietary | The MEMORY.md persistent-memory pattern, as a concept reference only. |

The design context for each credit lives in [`plan.md`](plan.md). The
milestone plans record where a scheme was adopted and what changed in
the reimplementation.
