// Reading a version's mesh.json (docs/PROTOCOL.md 3.4). The file is what
// three-cad-viewer renders; its `hotend` section maps the names of the model's parts to
// the viewer's paths, which is how a picked face is traced back to a part.

/** One entry of `shapes.parts`: only the fields the window reads or changes. */
export type MeshPart = {
  id: string;
  name: string;
  color?: string | string[];
  [field: string]: unknown;
};

export type MeshDocument = {
  shapes: { parts: MeshPart[]; [field: string]: unknown };
  hotend: { protocol: number; parts: Record<string, string>; [field: string]: unknown };
  [field: string]: unknown;
};

export type NamedPart = { name: string; path: string };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// three-cad-viewer writes the names and colours of a shape tree into the page as HTML
// (its tree view, which is built even while it is hidden) and builds selectors from
// them. The file is written by a sandboxed worker and is read as hostile input: nothing
// reaches the viewer that is not a name, a path or a colour of exactly these forms.
const ROOT = { name: "model", id: "/model" } as const;
/** A part of the model, as the engine names it (`hotend_engine.model.NAME_RE`). */
const PART_NAME = /^[a-z][a-z0-9_]{0,31}$/;
/** A reference drawn as a ghost: `ghost.<reference>`, `ghost.<reference>.<n>` for one more. */
const GHOST_NAME = /^ghost\.[a-z][a-z0-9_]{0,31}(\.[0-9]{1,4})?$/;
/** What the window itself adds for a plate (./scene.ts): the outline and the pieces. */
const PLATE_NAME = /^(hotend-plate|piece-[0-9]{1,4}-[a-z][a-z0-9_]{0,31})$/;
const COLOUR = /^#[0-9a-f]{6}$/i;
/** Any other text of the tree (`type`, `subtype`): one plain word. */
const WORD = /^[A-Za-z0-9_-]{0,32}$/;
/** More leaves than a model can have parts and ghosts, or a plate pieces. */
const MAX_LEAVES = 4096;

function plain(value: unknown, where: string): void {
  if (typeof value === "string") {
    if (!WORD.test(value)) throw new Error(`The mesh has text the 3D view does not take (${where}).`);
  } else if (Array.isArray(value)) {
    value.forEach((entry) => {
      plain(entry, where);
    });
  } else if (isRecord(value)) {
    for (const [key, entry] of Object.entries(value)) plain(entry, `${where}.${key}`);
  }
}

function checkShapes(shapes: unknown, leafName: (name: string) => boolean): asserts shapes is MeshDocument["shapes"] {
  if (!isRecord(shapes)) throw new Error("The mesh has no shapes.");
  const { name, id, parts, color, ...rest } = shapes;
  if (name !== ROOT.name || id !== ROOT.id || color !== undefined) throw new Error("The mesh does not start at /model.");
  if (!Array.isArray(parts) || parts.length > MAX_LEAVES) throw new Error("The mesh lists no parts.");
  plain(rest, "shapes");
  const seen = new Set<string>();
  for (const part of parts as unknown[]) {
    if (!isRecord(part)) throw new Error("A part of the mesh is not an object.");
    const { name: leaf, id: path, color: colour, ...others } = part;
    if (typeof leaf !== "string" || !leafName(leaf) || seen.has(leaf)) throw new Error("A part of the mesh has a name that no part can have.");
    seen.add(leaf);
    // One leaf per part, directly under the root: the path is the name, and nothing nests.
    if (path !== `${ROOT.id}/${leaf}` || "parts" in others) throw new Error(`The part ${leaf} of the mesh is not at /model/${leaf}.`);
    const colours = colour === undefined ? [] : Array.isArray(colour) ? (colour as unknown[]) : [colour];
    if (!colours.every((one) => typeof one === "string" && COLOUR.test(one))) throw new Error(`The part ${leaf} of the mesh has a colour that is not #RRGGBB.`);
    plain(others, leaf);
  }
}

/**
 * Throws unless a document the window is about to draw holds only names, paths and
 * colours the viewer may write into the page: the last check before the viewer, for the
 * engine's parts and for what the window added to them.
 */
export function checkScene(document: { shapes: unknown }): void {
  checkShapes(document.shapes, (name) => PART_NAME.test(name) || GHOST_NAME.test(name) || PLATE_NAME.test(name));
}

/** The mesh as written by the engine. Throws when it is not a mesh this build can show. */
export function parseMesh(text: string): MeshDocument {
  const document: unknown = JSON.parse(text);
  if (!isRecord(document) || !isRecord(document.shapes) || !isRecord(document.hotend)) {
    throw new Error("The mesh has no shapes or no hotend section.");
  }
  if (document.hotend.protocol !== 0) {
    throw new Error(`The mesh is of protocol ${String(document.hotend.protocol)}, not 0.`);
  }
  checkShapes(document.shapes, (name) => PART_NAME.test(name) || GHOST_NAME.test(name));
  const named = document.hotend.parts;
  if (!isRecord(named) || !Object.entries(named).every(([name, path]) => PART_NAME.test(name) && path === `${ROOT.id}/${name}`)) {
    throw new Error("The mesh does not say which part is which.");
  }
  return document as MeshDocument;
}

/** The model's parts in the order the mesh draws them. */
export function namedParts(mesh: MeshDocument): NamedPart[] {
  const names = new Map(Object.entries(mesh.hotend.parts).map(([name, path]) => [path, name]));
  return mesh.shapes.parts.flatMap((part) => {
    const name = names.get(part.id);
    return name === undefined ? [] : [{ name, path: part.id }];
  });
}

/**
 * The part that owns a viewer path such as `/model/box/faces/faces_3`, or null when the
 * path belongs to nothing the model named.
 */
export function partOfPath(mesh: MeshDocument, path: string): string | null {
  let best: NamedPart | null = null;
  for (const part of namedParts(mesh)) {
    const owns = path === part.path || path.startsWith(`${part.path}/`);
    if (owns && (best === null || part.path.length > best.path.length)) best = part;
  }
  return best?.name ?? null;
}

/** A CSS colour of the form #RRGGBB as the number three.js takes. */
export function colourNumber(css: string): number | null {
  const match = /^#([0-9a-f]{6})$/i.exec(css.trim());
  return match?.[1] === undefined ? null : Number.parseInt(match[1], 16);
}

/** Two #RRGGBB colours mixed, `share` of the second into the first, as three.js's number. */
export function mixColours(base: string, other: string, share: number): number | null {
  const [from, to] = [colourNumber(base), colourNumber(other)];
  if (from === null || to === null) return null;
  let mixed = 0;
  for (const shift of [16, 8, 0]) {
    const [a, b] = [(from >> shift) & 0xff, (to >> shift) & 0xff];
    mixed |= Math.round(a + (b - a) * share) << shift;
  }
  return mixed;
}
