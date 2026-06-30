# Changelog

All notable changes to codex-trace are documented here. Versions follow
[semantic versioning](https://semver.org/), and this file follows
[Keep a Changelog](https://keepachangelog.com/) conventions.

## [0.4.0] — 2026-06-28

A fresh app icon in codex green, a quieter macOS install, and a much lighter startup.
codex-trace no longer balloons memory while it scans your session history, and the macOS
bundle identifier no longer trips a system warning on launch.

### Added

- **Codex-green app icon**
  ([`1abd896`](https://github.com/PixelPaw-Labs/codex-trace/commit/1abd896)). The app
  icon's iris is recolored from orange to codex green (`#10a37f`) across every asset —
  the macOS `.icns`, the Windows `.ico` and Store tiles, and all PNG sizes — so the
  installed app, dock, and taskbar all show the new mark. The README header now carries
  the icon too.

### Fixed

- **Startup no longer spikes memory on large session histories**
  ([`20d85f4`](https://github.com/PixelPaw-Labs/codex-trace/commit/20d85f4)). The
  discovery scan used to load each session file fully into memory, so peak usage jumped
  to the size of your largest rollout file (often hundreds of MB) before settling. The
  scan now streams each file line by line — decompressing zstd on the fly — so memory
  during discovery is bounded to a single line regardless of session size.
- **macOS install no longer warns about the bundle identifier**
  ([`2b49ff9`](https://github.com/PixelPaw-Labs/codex-trace/commit/2b49ff9)). The bundle
  identifier ended in `.app`, which macOS flags as conflicting with the application
  bundle extension. It is now `com.codextrace.desktop`, so installing and launching the
  app is clean.

[0.4.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.4.0

## [0.3.0] — 2026-06-28

Patch tool calls now read like a real code review, and the parser keeps pace with the
newest Codex CLI releases (v0.140.0 and v0.141.0). If you saw raw `*** Begin Patch`
text instead of a diff, or sessions from the latest Codex builds showed missing context
tools, unrecognized MCP tool calls, or spurious turns around `/import`, this release
addresses those.

### Added

- **`apply_patch` renders as a red/green diff**
  ([`426ea62`](https://github.com/PixelPaw-Labs/codex-trace/commit/426ea62)). An
  `apply_patch` tool call now shows a per-file, per-hunk diff with `+`/`-` markers,
  red/green line tinting, and word-level highlighting on the spans that actually
  changed — instead of the raw patch body. It falls back to the previous
  `patch_changes` / raw views when the input isn't a recognizable patch.

### Fixed

- **Tool calls from the latest Codex builds are classified correctly**
  ([`83cc23b`](https://github.com/PixelPaw-Labs/codex-trace/commit/83cc23b)). Codex
  v0.141.0 emits dynamic tool namespaces (MCP, connector, plugin) in `ThreadStart` /
  `task_started` events, so calls now arrive as qualified `mcp:server/tool_name` names
  or need a registry lookup. codex-trace reads the `dynamic_tools` registry and parses
  the qualified format, so these tools are recognized as MCP calls rather than mislabeled.
- **Context-budget tools are recognized**
  ([`c212d71`](https://github.com/PixelPaw-Labs/codex-trace/commit/c212d71)). Codex
  v0.140.0's `token_budget_context`, `context_remaining`, and `context_window` calls
  are now classified as context queries instead of falling through as unknown tools.
- **`/import` sessions parse cleanly**
  ([`3f73060`](https://github.com/PixelPaw-Labs/codex-trace/commit/3f73060)). Codex
  v0.140.0's `/import` command writes new lifecycle entries (e.g.
  `external_agent_imported`) before the first `task_started`, and v0.141.0 adds an
  `external_agent_import_result` response item. These are now handled explicitly, so
  imported-agent context no longer produces spurious synthetic turns or corrupts the
  turn it sits in.
- **IPC commands are granted explicitly in the ACL**
  ([`7e330bd`](https://github.com/PixelPaw-Labs/codex-trace/commit/7e330bd)). The app
  previously relied on Tauri implicitly permitting its own commands. Each command is now
  granted through an explicit permission set, with a regression test that cross-checks
  the handlers against the ACL in both directions — closing a path where a wired-up
  command could fail at runtime with "Command not allowed by ACL".

## [0.2.0] — 2026-06-16

A readability upgrade for the turn view plus a sweep of parser compatibility with the
latest Codex CLI releases (v0.132.0 through v0.139.0). If your sessions had blank final
answers, missing memory notes, or tool calls that looked corrupted on newer Codex
builds, this release fixes those — and the assistant's commentary now reads inline,
in order, alongside the tool calls it interleaves with.

### Added

- **Assistant commentary renders inline**
  ([`20d48f4`](https://github.com/PixelPaw-Labs/codex-trace/commit/20d48f4)). The
  assistant's prose is now a first-class timeline item ("Complementary") shown expanded
  by default, so a turn reads commentary → tool call → commentary → … → final answer top
  to bottom instead of leaving the text as loose lines above a tool box.
- **Image file paths from generated images**
  ([`1deb184`](https://github.com/PixelPaw-Labs/codex-trace/commit/1deb184)). Codex
  v0.138.0 attaches a `file_path` to image-generation results; codex-trace now surfaces
  it so you can see where a generated image landed on disk.
- **Archived-session awareness**
  ([`fcc8bc4`](https://github.com/PixelPaw-Labs/codex-trace/commit/fcc8bc4)). Sessions
  archived or unarchived via Codex v0.136.0's `codex archive` / `/archive` are now
  tracked, so archived runs are recognized rather than shown as ordinary sessions.

### Fixed

- **Raw command output no longer corrupts tool-call details**
  ([`6850c30`](https://github.com/PixelPaw-Labs/codex-trace/commit/6850c30)). On Codex
  v0.133.0, exec output is kept verbatim; phrases like "exit code: 1" inside real output
  were being mistaken for metadata. Exec metadata is now read only from the structured
  `Output:` marker, so a compiler or test log can no longer fake an exit code or
  duration.
- **Tool calls with structured arguments are no longer dropped**
  ([`f081fd0`](https://github.com/PixelPaw-Labs/codex-trace/commit/f081fd0)). Codex
  v0.139.0 can emit `function_call` arguments as a JSON object rather than a string;
  those calls now parse and display instead of showing up empty.
- **Final answers from `--output-schema` runs now display**
  ([`26f1874`](https://github.com/PixelPaw-Labs/codex-trace/commit/26f1874)). Codex
  v0.132.0 `structured_output` / `message` response items were silently skipped, leaving
  the final answer blank; they're now shown.
- **Versioned memory summaries are parsed again**
  ([`947f248`](https://github.com/PixelPaw-Labs/codex-trace/commit/947f248)). Codex
  v0.132.0 made `turn_context` memories versioned objects instead of plain strings,
  which dropped them from the view; both forms are now handled.
- **Agent-interrupt events are recognized under their new name**
  ([`b9f9bd1`](https://github.com/PixelPaw-Labs/codex-trace/commit/b9f9bd1)). Codex
  v0.139.0 renamed `close_agent` to `interrupt_agent`; both names are now classified
  correctly, so multi-agent runs keep displaying these events.

### Changed

- **Fonts aligned with claude-code-trace**
  ([`20d48f4`](https://github.com/PixelPaw-Labs/codex-trace/commit/20d48f4)). Detail-view
  text now uses fixed `px` sizing (13px prose) instead of `rem`, so type no longer
  rescales with browser/OS root font settings.

[0.3.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.3.0
[0.2.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.2.0

## [0.1.0] — 2026-06-08

The first release of Codex Trace — a desktop app for browsing and inspecting your
local Codex CLI sessions. Point it at `~/.codex/sessions` and it parses the rollout
JSONL files into a date-grouped session list and a per-session detail view, so you can
read a run turn-by-turn instead of scrolling raw logs.

### Added

- **Session browser and detail view.** Sessions are discovered from
  `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, grouped by date in the sidebar, and
  opened into a turn-by-turn detail view. Tool calls render inline in chronological
  order, each with an inline summary after the call name so you can skim a run at a
  glance ([#100](https://github.com/PixelPaw-Labs/codex-trace/pull/100)).
- **Broad Codex CLI version coverage.** The parser understands rollout formats across
  many Codex releases — goal lifecycle events
  ([#73](https://github.com/PixelPaw-Labs/codex-trace/pull/73)), `UserInput` /
  `ThreadSettings` items ([#87](https://github.com/PixelPaw-Labs/codex-trace/pull/87)),
  MCP `plugin_id` ([#74](https://github.com/PixelPaw-Labs/codex-trace/pull/74)),
  `trace_id` / `forked_from_thread_id` / compaction metadata
  ([#94](https://github.com/PixelPaw-Labs/codex-trace/pull/94)), memory context from
  `turn_context` ([#95](https://github.com/PixelPaw-Labs/codex-trace/pull/95)),
  `shell_hook_output` events
  ([#113](https://github.com/PixelPaw-Labs/codex-trace/pull/113)), and subagent
  identity fields ([#114](https://github.com/PixelPaw-Labs/codex-trace/pull/114)).
- **Headless / Docker mode.** The app can run without a desktop WebView, making it
  usable on servers and in containers.
- **macOS app bundle installer.** Installing on macOS now produces a proper `.app`
  bundle rather than a bare binary
  ([#99](https://github.com/PixelPaw-Labs/codex-trace/pull/99)).
- **`cut-release` skill.** A project-local Claude Code skill that automates cutting,
  tagging, and publishing a release end-to-end.

### Fixed

- **Compressed rollouts are now readable.** zstd-compressed rollout files (Codex
  v0.137.0) are transparently decompressed instead of failing to parse
  ([#109](https://github.com/PixelPaw-Labs/codex-trace/pull/109)).
- **MCP tool calls resolve correctly.** Tool calls are resolved from `tool_id` in
  v0.130.0 sessions ([#44](https://github.com/PixelPaw-Labs/codex-trace/pull/44)) and
  `mcp_tool_call` turn items from v0.129.0 are handled
  ([#39](https://github.com/PixelPaw-Labs/codex-trace/pull/39)).
- **Image-generation calls are classified correctly** rather than showing as a generic
  tool call ([#112](https://github.com/PixelPaw-Labs/codex-trace/pull/112)).
- **Forward-compatibility guards.** Hidden spawn-agent metadata
  ([#111](https://github.com/PixelPaw-Labs/codex-trace/pull/111)) and `assign_task` /
  `followup_task` items
  ([#108](https://github.com/PixelPaw-Labs/codex-trace/pull/108)) from newer Codex
  builds are now recognised instead of silently dropped.

### Performance

- **No more full session-list streaming on every file-system event** — the session list
  updates incrementally instead of being re-sent on each change.
- **WebKit and Xvfb are skipped in headless/Docker mode**, cutting startup cost and
  dependencies where no GUI is needed.

[0.1.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.1.0
