// What the stage shows, as a document three-cad-viewer renders (DESIGN_SPEC 10.3). The
// engine's mesh is used as it is; the window only places copies of the parts for the
// plate, marks each part with the index of its tint, and adds the outline of the bed.
import type { MeshDocument, MeshPart } from "./mesh";

/** A part of a scene: a part of the mesh plus what the view needs to draw it. */
export type ScenePart = MeshPart & {
  /** Which tint the part takes: the index of the part in the model. */
  tint?: number;
  /** A token that gives the part its colour instead of a tint (the plate outline). */
  token?: string;
};

export type SceneDocument = Omit<MeshDocument, "shapes"> & {
  shapes: Omit<MeshDocument["shapes"], "parts"> & { parts: ScenePart[] };
};

/** One piece on a plate: which part of the model, and where it lies. */
export type Placed = {
  name: string;
  part: string;
  /** 12 numbers, a 3 x 4 matrix row by row: plate = R * model + t. */
  transform: readonly number[];
};

type Quaternion = [number, number, number, number];
type Location = [[number, number, number], Quaternion];

const PLATE_NAME = "hotend-plate";

/** The rotation of a 3 x 4 matrix (row by row) as a quaternion x, y, z, w. */
export function quaternionOf(m: readonly number[]): Quaternion {
  const [m00 = 1, m01 = 0, m02 = 0, , m10 = 0, m11 = 1, m12 = 0, , m20 = 0, m21 = 0, m22 = 1] = m;
  const trace = m00 + m11 + m22;
  if (trace > 0) {
    const s = Math.sqrt(trace + 1) * 2;
    return [(m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, s / 4];
  }
  if (m00 > m11 && m00 > m22) {
    const s = Math.sqrt(1 + m00 - m11 - m22) * 2;
    return [s / 4, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s];
  }
  if (m11 > m22) {
    const s = Math.sqrt(1 + m11 - m00 - m22) * 2;
    return [(m01 + m10) / s, s / 4, (m12 + m21) / s, (m02 - m20) / s];
  }
  const s = Math.sqrt(1 + m22 - m00 - m11) * 2;
  return [(m02 + m20) / s, (m12 + m21) / s, s / 4, (m10 - m01) / s];
}

function multiply(a: Quaternion, b: Quaternion): Quaternion {
  const [ax, ay, az, aw] = a;
  const [bx, by, bz, bw] = b;
  return [
    aw * bx + ax * bw + ay * bz - az * by,
    aw * by - ax * bz + ay * bw + az * bx,
    aw * bz + ax * by - ay * bx + az * bw,
    aw * bw - ax * bx - ay * by - az * bz,
  ];
}

function apply(m: readonly number[], v: readonly number[]): [number, number, number] {
  const [x = 0, y = 0, z = 0] = v;
  return [0, 1, 2].map(
    (row) => (m[row * 4] ?? 0) * x + (m[row * 4 + 1] ?? 0) * y + (m[row * 4 + 2] ?? 0) * z + (m[row * 4 + 3] ?? 0),
  ) as [number, number, number];
}

function locationOf(part: MeshPart): Location {
  const loc = part.loc as Location | null | undefined;
  return loc ?? [[0, 0, 0], [0, 0, 0, 1]];
}

/** Where a part of the model lies on the plate: the piece's transform after its own. */
export function placedLocation(part: MeshPart, transform: readonly number[]): Location {
  const [position, rotation] = locationOf(part);
  return [apply(transform, position), multiply(quaternionOf(transform), rotation)];
}

/**
 * The outline of the bed the main nozzle reaches, at Z 0, as an `edges` part. The view
 * turns around the origin, so the plate is drawn with its centre there.
 */
export function plateOutline(size: readonly [number, number]): ScenePart {
  const [x, y] = [size[0] / 2, size[1] / 2];
  const corners = [
    [-x, -y],
    [x, -y],
    [x, y],
    [-x, y],
  ] as const;
  const edges = corners.map(([ax, ay], index) => {
    const [bx, by] = corners[(index + 1) % corners.length] ?? [0, 0];
    return [ax, ay, 0, bx, by, 0];
  });
  return {
    id: `/model/${PLATE_NAME}`,
    type: "edges",
    name: PLATE_NAME,
    shape: { edges, obj_vertices: [] },
    state: [3, 1],
    width: 1.5,
    loc: [[0, 0, 0], [0, 0, 0, 1]],
    renderback: false,
    token: "--hotend-stage-plate",
  };
}

/** The parts as modelled and assembled (Design). */
export function designScene(mesh: MeshDocument): SceneDocument {
  return {
    ...mesh,
    shapes: { ...mesh.shapes, parts: mesh.shapes.parts.map((part, tint) => ({ ...part, tint })) },
  };
}

/**
 * The plate with the pieces lying on it (Prepare, Print), or the empty plate when there
 * is no mesh or no layout. Copies of a part share the part's mesh.
 */
export function plateScene(
  mesh: MeshDocument | null,
  size: readonly [number, number],
  pieces: readonly Placed[],
  height = 10,
): SceneDocument {
  const names = new Map(Object.entries(mesh?.hotend.parts ?? {}));
  const parts: ScenePart[] = [];
  const paths: Record<string, string> = {};
  let top = height;
  pieces.forEach((piece, index) => {
    const path = names.get(piece.part);
    const tint = mesh?.shapes.parts.findIndex((part) => part.id === path) ?? -1;
    const source = tint < 0 ? undefined : mesh?.shapes.parts[tint];
    if (!source) return;
    // The viewer writes a leaf's name into the page (./mesh.ts, `checkScene`). The name
    // it gets is the window's own, made of the piece's number and a part the mesh named;
    // the piece's name from the layout stays in `hotend.parts`, which only Hotend reads.
    const name = `piece-${index + 1}-${piece.part}`;
    const id = `/model/${name}`;
    const [at, turned] = placedLocation(source, piece.transform);
    // The layout counts from a corner of the plate; the view from its centre.
    parts.push({ ...source, id, name, loc: [[at[0] - size[0] / 2, at[1] - size[1] / 2, at[2]], turned], tint });
    paths[piece.name] = id;
    const bb = mesh?.shapes.bb as { zmax?: number; zmin?: number; xmax?: number; xmin?: number; ymax?: number; ymin?: number } | undefined;
    const reach = Math.max(
      (bb?.zmax ?? 0) - (bb?.zmin ?? 0),
      (bb?.xmax ?? 0) - (bb?.xmin ?? 0),
      (bb?.ymax ?? 0) - (bb?.ymin ?? 0),
    );
    top = Math.max(top, reach);
  });
  return {
    ...(mesh ?? { instances: [] }),
    shapes: {
      version: 3,
      name: "model",
      id: "/model",
      loc: null,
      normal_len: 0,
      ...mesh?.shapes,
      parts: [plateOutline(size), ...parts],
      bb: { xmin: -size[0] / 2, xmax: size[0] / 2, ymin: -size[1] / 2, ymax: size[1] / 2, zmin: 0, zmax: top },
    },
    hotend: { protocol: 0, ...mesh?.hotend, parts: paths },
  };
}

/** The scene with every tint index and token replaced by a colour. */
export function coloured(
  scene: SceneDocument,
  tints: readonly string[],
  tokenColour: (token: string) => string,
): SceneDocument {
  return {
    ...scene,
    shapes: {
      ...scene.shapes,
      parts: scene.shapes.parts.map(({ tint, token, ...part }) => {
        if (token !== undefined) return { ...part, color: tokenColour(token) };
        if (tint === undefined || tints.length === 0) return part;
        return { ...part, color: tints[tint % tints.length] };
      }),
    },
  };
}
