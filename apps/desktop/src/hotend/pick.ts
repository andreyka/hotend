// What a click on the part is called in words (DESIGN_SPEC 4.3.2). The viewer reports the
// point that was hit; the engine reports each part's bounding box. A point on the box's
// top is "the top", one on a wall that runs the part's longer way is "a long side", and
// one that lies inside the box is on an inside face. The coordinates stay out of what the
// maker reads; the message to Hotend carries them in brackets.
import { S } from "./strings";

export type Box = { min: readonly number[]; max: readonly number[] };
export type PickKind = "face" | "edge" | "vertex";

/** How close to the box's side a point counts as on it, in millimetres. */
const NEAR = 0.05;

type Place = "top" | "bottom" | "long" | "short" | "side" | "inside";

function place(point: readonly number[], box: Box): Place {
  const [x = 0, y = 0, z = 0] = point;
  const on = (value: number, axis: number, end: "min" | "max") => Math.abs(value - (box[end][axis] ?? 0)) <= NEAR;
  if (on(z, 2, "max")) return "top";
  if (on(z, 2, "min")) return "bottom";
  const long = [(box.max[0] ?? 0) - (box.min[0] ?? 0), (box.max[1] ?? 0) - (box.min[1] ?? 0)];
  const [alongX = 0, alongY = 0] = long;
  // A wall at the end of X runs along Y, and the other way round.
  const onX = on(x, 0, "min") || on(x, 0, "max");
  const onY = on(y, 1, "min") || on(y, 1, "max");
  if (!onX && !onY) return "inside";
  if (onX && onY) return "side";
  if (Math.abs(alongX - alongY) <= NEAR) return "side";
  const runs = onX ? alongY : alongX;
  return runs >= Math.max(alongX, alongY) - NEAR ? "long" : "short";
}

/** "the top", "a long side", "an edge at the top": where on its part a click landed. */
export function pickedPlace(kind: PickKind, point: readonly number[] | null, box: Box | null): string {
  const words = S.stage.places;
  if (point === null || box === null) return words.unknown[kind];
  return words[kind][place(point, box)];
}
