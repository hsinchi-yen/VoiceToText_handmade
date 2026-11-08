# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

All shared project rules, commands, and workflow are in AGENTS.md — follow it:

@AGENTS.md

## Claude Code specifics

- **Orchestrator role** (the main session): pick ready cards from `tasks/cards/` (prerequisites merged, see the wave table in `tasks/cards/README.md`), and run at most **3 implementation subagents + 1 reviewer** at a time. Launch implementation agents with `isolation: "worktree"`, and give each agent its card path plus the doc sections the card links. Don't paste whole documents.
- **Reviewer**: a separate subagent given only the diff and the card. Don't give it the implementer's summary. It checks acceptance criteria, scope (only allowed paths touched), the no-transcript-logging rule, and tests.
- Stop and ask the user before: merging any change to a `v2c-core` public interface, closing a phase checkpoint, or merging a card that has 🧑 items.
- Reply to the user in Traditional Chinese (Taiwan).
