// The frame's pieces that need no core: the error boundary and the theme.
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ErrorBoundary } from "@/host/ErrorBoundary";
import { resolveTheme } from "@/host/theme";

afterEach(cleanup);

function Broken({ broken }: { broken: { now: boolean } }) {
  if (broken.now) throw new Error("the screen broke");
  return <p>the screen</p>;
}

describe("the error boundary", () => {
  it("shows what went wrong instead of a blank window, and can try again", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const broken = { now: true };
    render(
      <ErrorBoundary>
        <Broken broken={broken} />
      </ErrorBoundary>,
    );
    expect(screen.getByRole("alert").textContent).toContain("This screen could not be shown.");
    expect(screen.getByRole("alert").textContent).toContain("the screen broke");

    broken.now = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(screen.getByText("the screen")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("the theme", () => {
  it("resolves System to the system's theme and keeps an explicit choice", () => {
    expect(resolveTheme("system", "light")).toBe("light");
    expect(resolveTheme("system", "dark")).toBe("dark");
    expect(resolveTheme("dark", "light")).toBe("dark");
    expect(resolveTheme("light", "dark")).toBe("light");
  });
});
