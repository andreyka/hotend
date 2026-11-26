// Number field and stepper: sizes, copies, effort (DESIGN_SPEC 8), on Base UI's
// NumberField, styled like the text input.
import { NumberField as NumberFieldPrimitive } from "@base-ui/react/number-field";
import { Minus, Plus } from "lucide-react";

import { cn } from "@/lib/utils";

/**
 * What was typed or pasted, with a point as the decimal mark. Half the world
 * writes 118,5: read with a point only, the field kept "118" and dropped the
 * rest without a word. One comma is a decimal comma; where a point is there as
 * well, the mark that comes last is the decimal mark and the other one groups.
 */
export function decimalPoint(text: string): string {
  if (!text.includes(",")) return text;
  const commas = text.split(",").length - 1;
  if (!text.includes(".")) return commas === 1 ? text.replace(",", ".") : text.replaceAll(",", "");
  if (text.lastIndexOf(",") > text.lastIndexOf(".") && commas === 1) {
    return text.replaceAll(".", "").replace(",", ".");
  }
  return text.replaceAll(",", "");
}

/** Writes the text into the input the way typing does, so the field hears of it. */
function write(input: HTMLInputElement, text: string) {
  const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  set?.call(input, text);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

type Props = Omit<NumberFieldPrimitive.Root.Props, "className"> & {
  label: string;
  className?: string;
  /** With the minus and plus buttons of a stepper. */
  stepper?: boolean;
  inputClassName?: string;
  /** The value cannot be used as it is: the frame is drawn in the colour of a failure. */
  invalid?: boolean;
};

export function NumberField({ label, className, stepper = false, inputClassName, invalid = false, format, ...props }: Props) {
  return (
    // The window writes every number with a point, whatever the machine's locale, and
    // never groups thousands: a comma in the field is then always one the maker typed, and
    // is read as a decimal comma.
    <NumberFieldPrimitive.Root locale="en-US" className={cn("inline-flex", className)} format={{ useGrouping: false, ...format }} {...props}>
      <NumberFieldPrimitive.Group
        className={cn(
          "app-ring-group inline-flex min-h-7 items-stretch rounded-control border bg-app-raised data-[disabled]:opacity-40",
          invalid ? "border-app-fail" : "border-app-control",
        )}
      >
        {stepper ? (
          <NumberFieldPrimitive.Decrement
            aria-label={`${label}: one fewer`}
            className="inline-flex w-7 items-center justify-center rounded-l-control text-app-text-2 hover:bg-app-hover hover:text-app-text disabled:opacity-40"
          >
            <Minus className="size-3.5" strokeWidth={1.5} />
          </NumberFieldPrimitive.Decrement>
        ) : null}
        <NumberFieldPrimitive.Input
          aria-label={label}
          aria-invalid={invalid || undefined}
          onChange={(event) => {
            // Before the field reads it: a decimal comma becomes a point.
            const input = event.currentTarget;
            const text = decimalPoint(input.value);
            if (text === input.value) return;
            const caret = input.selectionStart;
            const same = text.length === input.value.length;
            input.value = text;
            const at = same && caret !== null ? caret : text.length;
            input.setSelectionRange(at, at);
          }}
          onPaste={(event) => {
            const pasted = event.clipboardData.getData("text/plain");
            if (!pasted.includes(",")) return;
            const input = event.currentTarget;
            if (input.readOnly || input.disabled) return;
            event.preventDefault();
            event.preventBaseUIHandler();
            const from = input.selectionStart ?? input.value.length;
            const to = input.selectionEnd ?? from;
            write(input, decimalPoint(input.value.slice(0, from) + pasted.trim() + input.value.slice(to)));
          }}
          onKeyDown={(event) => {
            if (event.key === ",") {
              // A second decimal mark is not typed.
              const input = event.currentTarget;
              const from = input.selectionStart ?? 0;
              const to = input.selectionEnd ?? from;
              const rest = input.value.slice(0, from) + input.value.slice(to);
              if (rest.includes(".") || rest.includes(",")) event.preventDefault();
              return;
            }
            // Enter commits what was typed, as leaving the field does, and stays in it.
            if (event.key !== "Enter") return;
            const input = event.currentTarget;
            input.blur();
            input.focus();
          }}
          className={cn(
            "t-ui-strong num w-14 min-w-0 bg-transparent px-1.5 text-right text-app-text",
            stepper && "w-9 px-0 text-center",
            inputClassName,
          )}
        />
        {stepper ? (
          <NumberFieldPrimitive.Increment
            aria-label={`${label}: one more`}
            className="inline-flex w-7 items-center justify-center rounded-r-control text-app-text-2 hover:bg-app-hover hover:text-app-text disabled:opacity-40"
          >
            <Plus className="size-3.5" strokeWidth={1.5} />
          </NumberFieldPrimitive.Increment>
        ) : null}
      </NumberFieldPrimitive.Group>
    </NumberFieldPrimitive.Root>
  );
}
