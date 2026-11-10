# AGENTS.md

Shared rules for every AI agent (Claude Code, Codex, …) working in this repository.

## What this project is

**v2c**: a Typeless-style voice input tool. The user holds a hotkey, speaks Traditional Chinese (Taiwan) and/or English, and the polished text is typed at the cursor. A second hotkey speaks Chinese and outputs English. Local models by default; cloud APIs are opt-in. Windows first, then Android, then Linux (GNOME Wayland).

## Source-of-truth documents (read before starting any task)

| Document | Contains | Precedence |
|----------|----------|------------|
| `v2cplan.md` | Requirements, decisions, risks (the *what/why*) | 1 — wins on conflict |
| `tasks/plan.md` | Architecture, interfaces, per-module design (the *how*) | 2 |
| `tasks/cards/T*.md` | One task: goal, scope, acceptance | 3 |
| `tasks/todo.md` | Progress checklist | — |
| `tasks/human-checklist.md` | Steps only the human can do | — |

These documents are written in Traditional Chinese. If a card contradicts `v2cplan.md` or `tasks/plan.md`, stop and report it; do not pick one silently.

## Repository layout

```
crates/v2c-core       # pure logic + providers, NO OS-specific deps (must compile for Android)
crates/v2c-asr-local  # whisper-rs (Breeze-ASR-25, CUDA) + SenseVoice; desktop only
crates/v2c-platform   # ALL OS-specific code: {windows,linux}/{hotkey,inject,foreground}.rs
crates/v2c-ffi        # uniffi bindings for Android
crates/v2c-cli        # dev tool: transcribe / bench / hotkey-test / inject-test
apps/desktop          # Tauri v2; calls v2c-platform for anything OS-specific
apps/android          # Kotlin IME
tools/                # setup-dev.ps1, convert-breeze/
tests/fixtures/audio  # WAVs are git-ignored (the user's voice); only .txt expectations + README are tracked
```

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo test -p v2c-core trigger::            # single module
cargo test -p v2c-core trigger::tests::double_tap_enters_hands_free   # single test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p v2c-cli -- bench --asr breeze tests/fixtures/audio
cd apps/desktop && npm run tauri dev        # desktop app (after Phase 1 scaffolding exists)
```

Commands for things that don't exist yet are the intended ones; the task that creates them must make them work.

## Non-negotiable rules

1. **Never log transcribed text, polished text, or vocabulary.** Log lengths, timings, provider names, and error codes only. History text is encrypted at rest.
2. **`v2c-core` has no OS-specific dependencies.** Platform code lives only in `crates/v2c-platform`. Hotkey and injection *decisions* stay in pure, unit-tested modules (`trigger.rs`, `inject_plan.rs`).
3. **Local first.** Nothing may require an API key to work out of the box. Model names, endpoints, and thresholds go in config, not code.
4. **Stay inside the card's scope.** Edit only the paths the card lists under "允許修改". To change a `v2c-core` public interface, stop and request approval (see Workflow).
5. **Do not install system software** (CUDA, drivers, winget packages). Write the script or instructions; the human runs them.
   Keep every test artifact (downloaded test models, generated WAVs, bench output, scratch dirs) under `tests/` in this repo — use `tests/tmp/` (git-ignored) for throwaway files. Never create test folders in the user's home directory. The only build directory outside the repo is `C:\Users\lance.tn\.v2ct` (`CARGO_TARGET_DIR`); share it instead of creating per-task target dirs.
6. Verify any crate or external API marked 【實作前確認】 against its current docs before using it, and note the result in your report.

## Workflow

1. Take one card from `tasks/cards/`. Check its prerequisites are merged.
2. Work on branch `t<id>-<slug>` (e.g. `t0.2-trigger`) in your own git worktree.
3. Done means: acceptance criteria met, `cargo test` + `cargo clippy -D warnings` + `cargo fmt --check` pass, and `tasks/todo.md` is updated.
4. Items marked **🧑 需人工** cannot be done by an agent. Finish everything else, add them to `tasks/human-checklist.md`, then stop and report.
5. Report in Traditional Chinese: what changed, how it was verified (commands + results), what is left for the human, and any doc conflicts found.
6. Merge gate: tests/clippy pass **and** an independent reviewer agent approves. Leaf tasks may be merged by the orchestrator. Changes to `v2c-core` public interfaces, phase checkpoints, and tasks with 🧑 items need the human's approval.

## Language

Code, identifiers, comments, commit messages, and this file: English. Task cards, reports to the user, and user-facing docs (`docs/`, UI strings): Traditional Chinese (Taiwan).
