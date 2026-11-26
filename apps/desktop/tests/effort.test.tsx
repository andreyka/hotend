// The effort level in Settings: next to the model, `high` unless chosen, and honest about
// when the core leaves it out (the core's egress has the same table).
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import providerData from "@/host/data/providers.json";
import { EffortRow, effortNote, firstModel } from "@/host/sections";
import { H } from "@/host/strings";

afterEach(cleanup);

describe("the model suggestions", () => {
  it("start with the default model of a new installation", () => {
    expect(providerData.providers.anthropic.models).toEqual([
      "claude-fable-5-2",
      "claude-opus-5-5",
      "claude-sonnet-5-5",
      "claude-haiku-5-5",
    ]);
    expect(firstModel("anthropic")).toBe("claude-fable-5-2");
  });
});

describe("the effort level", () => {
  it("says it is sent for a documented model and level", () => {
    expect(effortNote({ kind: "anthropic", model: "claude-fable-5-2" }, "high")).toBe(H.ai.thinkingSent);
    expect(effortNote({ kind: "anthropic", model: " claude-opus-5-5 " }, "max")).toBe(H.ai.thinkingSent);
  });

  it("says it is left out for an unknown model, a level the model lacks, or another provider", () => {
    expect(effortNote({ kind: "anthropic", model: "my-own-model" }, "high")).toBe(H.ai.thinkingNotForModel);
    expect(effortNote({ kind: "anthropic", model: "claude-opus-4-6" }, "xhigh")).toBe(H.ai.thinkingLevelNotForModel);
    expect(effortNote({ kind: "openai", model: "gpt-5.1" }, "high")).toBe(H.ai.thinkingNotForProvider);
    expect(effortNote(null, "high")).toBe(H.ai.thinkingNotForProvider);
  });

  it("shows the chosen level and its note in the settings row", () => {
    render(<EffortRow provider={{ kind: "anthropic", model: "claude-fable-5-2" }} value="high" onChange={() => undefined} />);
    expect(screen.getByTestId("effort-note").textContent).toBe(H.ai.thinkingSent);
    expect(screen.getAllByText(H.ai.thinking).length).toBeGreaterThan(0);
    expect(document.body.textContent).toContain("High");
  });
});
