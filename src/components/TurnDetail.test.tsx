import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { AgentMessage, CodexTurn, TokenInfo } from "../../shared/types";
import { TurnDetail } from "./TurnDetail";

const TOKEN_INFO: TokenInfo = {
  input_tokens: 38_000,
  cached_input_tokens: 12_000,
  output_tokens: 2_000,
  reasoning_output_tokens: 500,
  total_tokens: 40_000,
  context_window_tokens: 26_000,
  model_context_window: 100_000,
};

const FINAL_MSG: AgentMessage = {
  text: "Done",
  phase: "final_answer",
  timestamp: "2026-04-26T10:01:00Z",
  is_reasoning: false,
};

function makeTurn(overrides: Partial<CodexTurn> = {}): CodexTurn {
  return {
    turn_id: "turn-1",
    started_at: 1745661600,
    completed_at: 1745661660,
    duration_ms: 60000,
    status: "complete",
    user_message: "Hello Codex",
    agent_messages: [FINAL_MSG],
    tool_calls: [],
    final_answer: "Done",
    total_tokens: TOKEN_INFO,
    model: "gpt-5.4",
    cwd: null,
    reasoning_effort: null,
    error: null,
    has_compaction: false,
    thread_name: null,
    collab_spawns: [],
    trace_id: null,
    forked_from_thread_id: null,
    compaction_meta: null,
    ...overrides,
  };
}

function renderTurnDetail(turn: CodexTurn) {
  render(<TurnDetail turn={turn} expanded={new Set()} onToggle={vi.fn()} onBack={vi.fn()} />);
}

describe("TurnDetail", () => {
  it("shows context-left metadata using Codex's last-token usage", () => {
    renderTurnDetail(makeTurn());

    expect(screen.getByText("ctx 84% left")).toBeInTheDocument();
    expect(document.querySelector(".info-bar__context-fill")).toHaveStyle({ width: "16%" });
    expect(screen.getByText("40.0k tok · 1m")).toBeInTheDocument();
  });

  it("omits context-left metadata when last-token usage is unavailable", () => {
    renderTurnDetail(
      makeTurn({
        total_tokens: {
          ...TOKEN_INFO,
          context_window_tokens: null,
        },
      }),
    );

    expect(screen.queryByText(/ctx .* left/)).not.toBeInTheDocument();
    expect(screen.getByText("40.0k tok · 1m")).toBeInTheDocument();
  });
});
