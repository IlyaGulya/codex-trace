import { useCallback, useState } from "react";
import type { CodexToolCall } from "../../shared/types";
import { formatDuration } from "../../shared/format";
import { formatJson } from "../lib/format";
import {
  ExecIcon,
  McpIcon,
  PatchIcon,
  WebIcon,
  ImageIcon,
  SpawnIcon,
  WaitIcon,
  CloseAgentIcon,
  FollowupTaskIcon,
  UnknownToolIcon,
  WarningIcon,
  PopoutIcon,
} from "./Icons";
import { PopoutModal } from "./PopoutModal";

interface ToolCallItemProps {
  tool: CodexToolCall;
  expanded: boolean;
  onToggle: () => void;
  isWorkerOpen?: boolean;
  onOpenWorker?: (tool: CodexToolCall) => void;
}

function kindIcon(kind: CodexToolCall["kind"], failed: boolean) {
  if (failed) return <WarningIcon />;
  switch (kind) {
    case "exec_command":
      return <ExecIcon />;
    case "mcp_tool":
      return <McpIcon />;
    case "patch_apply":
      return <PatchIcon />;
    case "web_search":
      return <WebIcon />;
    case "image_generation":
      return <ImageIcon />;
    case "spawn_agent":
      return <SpawnIcon />;
    case "wait_agent":
      return <WaitIcon />;
    case "close_agent":
      return <CloseAgentIcon />;
    case "followup_task":
      return <FollowupTaskIcon />;
    default:
      return <UnknownToolIcon />;
  }
}

function kindClass(kind: CodexToolCall["kind"]): string {
  switch (kind) {
    case "exec_command":
      return "tool-call--exec";
    case "mcp_tool":
      return "tool-call--mcp";
    case "patch_apply":
      return "tool-call--patch";
    case "web_search":
      return "tool-call--web";
    case "image_generation":
      return "tool-call--image";
    case "spawn_agent":
    case "wait_agent":
    case "close_agent":
    case "followup_task":
      return "tool-call--collab";
    default:
      return "tool-call--unknown";
  }
}

function summaryText(tool: CodexToolCall): string | null {
  switch (tool.kind) {
    case "exec_command":
      return tool.command ? tool.command.join(" ") : null;
    case "web_search":
      return tool.web_query;
    case "image_generation":
      return tool.image_prompt;
    case "patch_apply":
      if (tool.patch_changes) {
        return Object.keys(tool.patch_changes).join(", ");
      }
      return null;
    case "mcp_tool": {
      const args = tool.arguments;
      if (args && typeof args === "object" && !Array.isArray(args)) {
        const first = Object.values(args as Record<string, unknown>)[0];
        return typeof first === "string" ? first : null;
      }
      return null;
    }
    default:
      return null;
  }
}

function looksLikeJson(s: string): boolean {
  const t = s.trimStart();
  if (t[0] !== "{" && t[0] !== "[") return false;
  try {
    JSON.parse(s);
    return true;
  } catch {
    return false;
  }
}

export function ToolCallItem({
  tool,
  expanded,
  onToggle,
  isWorkerOpen,
  onOpenWorker,
}: ToolCallItemProps) {
  const handleToggle = useCallback(() => onToggle(), [onToggle]);
  const [popout, setPopout] = useState(false);

  const failed =
    (tool.exit_code !== null && tool.exit_code !== 0) ||
    tool.patch_success === false ||
    tool.status === "failed";

  return (
    <div className={`tool-call ${kindClass(tool.kind)}${failed ? " tool-call--failed" : ""}`}>
      <div
        className="tool-call__header"
        onClick={handleToggle}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") handleToggle();
        }}
      >
        <span className="tool-call__chevron">{expanded ? "▼" : "▶"}</span>
        <span className="tool-call__icon">{kindIcon(tool.kind, failed)}</span>
        <span className="tool-call__name">
          {tool.kind === "mcp_tool" && tool.mcp_server ? (
            <>
              <span className="tool-call__mcp-prefix">MCP {tool.mcp_server}</span>
              {" / "}
              {tool.mcp_tool ?? tool.name}
            </>
          ) : (
            tool.name
          )}
        </span>
        {summaryText(tool) && <span className="tool-call__summary">{summaryText(tool)}</span>}
        {tool.exit_code !== null && (
          <span
            className={`tool-call__exit${tool.exit_code !== 0 ? " tool-call__exit--fail" : ""}`}
          >
            exit {tool.exit_code}
          </span>
        )}
        {tool.duration_secs !== null && (
          <span className="tool-call__duration">{formatDuration(tool.duration_secs * 1000)}</span>
        )}
        {tool.kind === "spawn_agent" && tool.worker_session && onOpenWorker && (
          <button
            className={`tool-call__worker-btn${isWorkerOpen ? " tool-call__worker-btn--open" : ""}`}
            onClick={(e) => {
              e.stopPropagation();
              onOpenWorker(tool);
            }}
            title={isWorkerOpen ? "Close worker panel" : "Open worker session"}
          >
            {isWorkerOpen ? "Close" : "Open"}
          </button>
        )}
        <button
          className={`tool-call__popout-btn${tool.duration_secs === null && !(tool.kind === "spawn_agent" && tool.worker_session && onOpenWorker) ? " tool-call__popout-btn--push" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            setPopout(true);
          }}
          title="View full content"
        >
          <PopoutIcon />
        </button>
      </div>

      {expanded && <ToolCallBody tool={tool} />}

      {popout && (
        <PopoutModal
          onClose={() => setPopout(false)}
          header={
            <>
              <span className="tool-call__icon">{kindIcon(tool.kind, failed)}</span>
              <span className="popout-modal__name">{tool.name}</span>
              {tool.exit_code !== null && (
                <span
                  className={`tool-call__exit${tool.exit_code !== 0 ? " tool-call__exit--fail" : ""}`}
                >
                  exit {tool.exit_code}
                </span>
              )}
            </>
          }
        >
          <ToolCallBody tool={tool} popout />
        </PopoutModal>
      )}
    </div>
  );
}

function ToolCallBody({ tool, popout = false }: { tool: CodexToolCall; popout?: boolean }) {
  const cls = popout ? "tool-call__body tool-call__body--popout" : "tool-call__body";
  return (
    <div className={cls}>
      {/* Input section */}
      {tool.kind === "exec_command" && (tool.command || tool.arguments) && (
        <div className="tool-call__section tool-call__section--input">
          <div className="tool-call__section-title">Command</div>
          {tool.command ? (
            <pre className="tool-call__block tool-call__cmd">{tool.command.join(" ")}</pre>
          ) : (
            <pre className="tool-call__block tool-call__json">
              <code>{formatJson(JSON.stringify(tool.arguments))}</code>
            </pre>
          )}
          {tool.cwd && <div className="tool-call__cwd">cwd: {tool.cwd}</div>}
        </div>
      )}

      {tool.kind === "mcp_tool" && (
        <div className="tool-call__section tool-call__section--input">
          <div className="tool-call__section-title">Input</div>
          <div className="tool-call__block tool-call__mcp-info">
            {tool.mcp_server && <span className="tool-call__mcp-server">{tool.mcp_server}</span>}
            {tool.mcp_tool && <span className="tool-call__mcp-tool"> / {tool.mcp_tool}</span>}
          </div>
          {tool.arguments && Object.keys(tool.arguments).length > 0 && (
            <pre className="tool-call__block tool-call__json">
              <code>{formatJson(JSON.stringify(tool.arguments))}</code>
            </pre>
          )}
        </div>
      )}

      {tool.kind === "patch_apply" && tool.patch_changes && (
        <div className="tool-call__section tool-call__section--input">
          <div className="tool-call__section-title">Changes</div>
          <div className="tool-call__block tool-call__patch">
            {Object.entries(tool.patch_changes).map(([file, change]) => (
              <div key={file} className="tool-call__patch-file">
                <span className={`tool-call__patch-type tool-call__patch-type--${change.type}`}>
                  {change.type}
                </span>{" "}
                {file}
                {change.unified_diff && (
                  <pre className="tool-call__block tool-call__diff">{change.unified_diff}</pre>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {tool.kind === "patch_apply" && !tool.patch_changes && tool.input_text && (
        <div className="tool-call__section tool-call__section--input">
          <div className="tool-call__section-title">Patch</div>
          <pre className="tool-call__block tool-call__diff">{tool.input_text}</pre>
        </div>
      )}

      {tool.kind === "web_search" && (
        <div className="tool-call__section tool-call__section--input">
          <div className="tool-call__section-title">Query</div>
          <div className="tool-call__block tool-call__web">
            {tool.web_query && <div>{tool.web_query}</div>}
            {tool.web_url && <div className="tool-call__web-url">{tool.web_url}</div>}
          </div>
        </div>
      )}

      {tool.kind === "image_generation" && tool.image_prompt && (
        <div className="tool-call__section tool-call__section--input">
          <div className="tool-call__section-title">Prompt</div>
          <div className="tool-call__block tool-call__image-prompt">{tool.image_prompt}</div>
        </div>
      )}

      {(tool.kind === "spawn_agent" ||
        tool.kind === "wait_agent" ||
        tool.kind === "close_agent" ||
        tool.kind === "followup_task") &&
        Object.keys(tool.arguments ?? {}).length > 0 && (
          <div className="tool-call__section tool-call__section--input">
            <div className="tool-call__section-title">Arguments</div>
            <pre className="tool-call__block tool-call__json">
              <code>{formatJson(JSON.stringify(tool.arguments))}</code>
            </pre>
          </div>
        )}

      {tool.kind === "unknown" &&
        tool.arguments != null &&
        Object.keys(tool.arguments).length > 0 && (
          <div className="tool-call__section tool-call__section--input">
            <div className="tool-call__section-title">Input</div>
            <pre className="tool-call__block tool-call__json">
              <code>{formatJson(JSON.stringify(tool.arguments))}</code>
            </pre>
          </div>
        )}

      {tool.output !== null && (
        <div className="tool-call__section tool-call__section--output">
          <div className="tool-call__section-title">Output</div>
          <pre
            className={`tool-call__output${tool.exit_code !== null && tool.exit_code !== 0 ? " tool-call__output--error" : ""}`}
          >
            {looksLikeJson(tool.output) ? <code>{formatJson(tool.output)}</code> : tool.output}
          </pre>
        </div>
      )}
    </div>
  );
}
