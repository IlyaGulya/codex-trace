import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SessionLoading } from "./SessionLoading";

describe("SessionLoading", () => {
  it("shows indeterminate text when there is no progress", () => {
    render(<SessionLoading progress={null} />);
    expect(screen.getByText("Loading session…")).toBeInTheDocument();
    expect(document.querySelector(".app__loading-bar")).not.toBeInTheDocument();
  });

  it("shows byte progress when progress is available", () => {
    render(<SessionLoading progress={{ path: "/x", done: 1024, total: 2048 }} />);
    expect(screen.getByText(/Loading session… 1\.0 KB \/ 2\.0 KB/)).toBeInTheDocument();
    expect(document.querySelector(".app__loading-bar")).toBeInTheDocument();
  });

  it("clamps the progress bar to 100% when done exceeds total", () => {
    render(<SessionLoading progress={{ path: "/x", done: 9999, total: 100 }} />);
    const fill = document.querySelector<HTMLElement>(".app__loading-fill");
    expect(fill).toBeInTheDocument();
    expect(fill!.style.width).toBe("100%");
  });

  it("hides the bar when total is zero", () => {
    render(<SessionLoading progress={{ path: "/x", done: 5, total: 0 }} />);
    expect(screen.getByText("Loading session…")).toBeInTheDocument();
    expect(document.querySelector(".app__loading-bar")).not.toBeInTheDocument();
  });
});
