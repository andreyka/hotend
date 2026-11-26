// A decimal comma is a decimal mark: "118,5" used to be read as 118 without a word.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { decimalPoint, NumberField } from "@/components/ui/number-field";

afterEach(cleanup);

describe("the decimal mark of a typed number", () => {
  it("reads one comma as a decimal comma", () => {
    expect(decimalPoint("118,5")).toBe("118.5");
    expect(decimalPoint("42,5")).toBe("42.5");
    expect(decimalPoint(",5")).toBe(".5");
    expect(decimalPoint("-3,25")).toBe("-3.25");
  });

  it("leaves a number with a point alone", () => {
    expect(decimalPoint("118.5")).toBe("118.5");
    expect(decimalPoint("120")).toBe("120");
    expect(decimalPoint("")).toBe("");
  });

  it("takes the last mark as the decimal mark where both are written", () => {
    expect(decimalPoint("1.234,5")).toBe("1234.5");
    expect(decimalPoint("1,234.5")).toBe("1234.5");
    expect(decimalPoint("1,234,567")).toBe("1234567");
  });
});

function Field({ onCommit }: { onCommit(value: number | null): void }) {
  const [value, setValue] = useState<number | null>(120);
  return (
    <NumberField
      label="Inside width"
      value={value}
      onValueChange={setValue}
      onValueCommitted={onCommit}
      format={{ maximumFractionDigits: 2, useGrouping: false }}
    />
  );
}

describe("a number field", () => {
  for (const [typed, read] of [
    ["118,5", 118.5],
    ["42,5", 42.5],
    ["118.5", 118.5],
  ] as const) {
    it(`commits ${typed} as ${read}`, async () => {
      const onCommit = vi.fn();
      render(<Field onCommit={onCommit} />);
      const input = screen.getByLabelText("Inside width") as HTMLInputElement;
      await act(async () => {
        fireEvent.focus(input);
        fireEvent.change(input, { target: { value: typed } });
        await Promise.resolve();
      });
      // The field shows what will be used while it is still being typed.
      expect(input.value).toBe(String(read));
      await act(async () => {
        fireEvent.blur(input);
        await Promise.resolve();
      });
      expect(onCommit).toHaveBeenLastCalledWith(read, expect.anything());
      expect(input.value).toBe(String(read));
    });
  }

  it("reads a pasted number with a decimal comma", async () => {
    const onCommit = vi.fn();
    render(<Field onCommit={onCommit} />);
    const input = screen.getByLabelText("Inside width") as HTMLInputElement;
    await act(async () => {
      fireEvent.focus(input);
      input.setSelectionRange(0, input.value.length);
      fireEvent.paste(input, { clipboardData: { getData: () => "118,5" } });
      await Promise.resolve();
    });
    expect(input.value).toBe("118.5");
    await act(async () => {
      fireEvent.blur(input);
      await Promise.resolve();
    });
    expect(onCommit).toHaveBeenLastCalledWith(118.5, expect.anything());
  });
});

describe("a number of a thousand or more", () => {
  it("is shown without a grouping comma, so a comma is never misread when the number is edited", async () => {
    function Big() {
      const [value, setValue] = useState<number | null>(1234);
      return <NumberField label="Length" value={value} onValueChange={setValue} />;
    }
    render(<Big />);
    const input = screen.getByLabelText("Length") as HTMLInputElement;
    expect(input.value).toBe("1234");
    await act(async () => {
      fireEvent.focus(input);
      fireEvent.change(input, { target: { value: "12345" } });
      fireEvent.blur(input);
      await Promise.resolve();
    });
    expect(input.value).toBe("12345");
  });
});
