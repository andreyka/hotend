import type { ApiError } from "./bindings";

/** True for the error value every command rejects with. */
export function isApiError(value: unknown): value is ApiError {
  if (typeof value !== "object" || value === null) return false;
  const error = value as Record<string, unknown>;
  return (
    typeof error.kind === "string" &&
    typeof error.message === "string" &&
    typeof error.next_step === "string"
  );
}

/** Anything that was thrown, as an error the interface can show. */
export function toApiError(thrown: unknown): ApiError {
  if (isApiError(thrown)) return thrown;
  // A request that never reached the core: the browser says "Failed to fetch" or
  // "Load failed", which says nothing to a maker.
  if (thrown instanceof TypeError && /fetch|load failed|network/i.test(thrown.message)) {
    return {
      kind: "internal",
      message: "Hotend's core is not answering.",
      next_step: "Restart Hotend. If it keeps happening, open Settings, This computer.",
    };
  }
  const message = thrown instanceof Error ? thrown.message : String(thrown);
  return {
    kind: "internal",
    message,
    next_step: "Try again. If it keeps happening, restart Hotend.",
  };
}
