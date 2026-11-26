// The 3D view: three-cad-viewer 5.1.3 with its own toolbar, tree and tabs off
// (docs/ux/DESIGN_SPEC.md section 10). It draws a scene (./scene.ts) and reports what the
// maker clicks, as a part of the model.
import { useEffect, useImperativeHandle, useRef, useState, type Ref } from "react";
import {
  Display,
  Viewer,
  type QuaternionTuple,
  type Shapes,
  type Vector3Tuple,
} from "three-cad-viewer";
import "three-cad-viewer/css";

import { checkScene, colourNumber, mixColours, namedParts, partOfPath, type MeshDocument } from "./mesh";
import { coloured, type SceneDocument } from "./scene";
import "./viewport.css";

export type StandardView = "iso" | "front" | "rear" | "left" | "right" | "top" | "bottom";

/** What was clicked: a face or an edge of a part, and where on it. */
export type Picked = {
  /** The part's name in the model, from the mesh's `hotend.parts` map. */
  part: string;
  kind: "face" | "edge" | "vertex";
  /** The viewer's path of the face or edge, e.g. `/model/box/faces/faces_3`. */
  path: string;
  /** The point that was hit, in model millimetres. Null when the GPU cannot tell. */
  point: [number, number, number] | null;
};

/**
 * Something Hotend points at on the part, drawn over the view in the accent and moved
 * with the camera: the place of a fault with its measurement ("0.6 mm"), or a dimension
 * from one point to another ("120 mm").
 */
export type ViewMark =
  | { kind: "spot"; at: readonly [number, number, number]; label: string | null }
  | { kind: "span"; from: readonly [number, number, number]; to: readonly [number, number, number]; label: string | null }
  /** The outline of a box in the scene, as the camera sees it: a piece on the plate. */
  | { kind: "box"; min: readonly [number, number, number]; max: readonly [number, number, number] };

/** One cut through the scene: everything beyond `at` along the axis is not drawn. */
export type Clip = { axis: "x" | "y" | "z"; at: number };

export type ViewportHandle = {
  /** Brings every visible part into view without turning the camera. */
  fit(): void;
  /** Turns the camera to a standard view and fits. */
  view(direction: StandardView): void;
  /** What is under a point of the view, in CSS pixels from its top left corner. */
  pickAt(x: number, y: number): Picked | null;
  /**
   * A picture of the part for a project card, as a PNG data URL with nothing behind the
   * part: always from the same corner and fitted to the card's 3:2, without the grid and
   * the axis marker, whatever the maker's own view is. One picture in the colours of each
   * theme, so that a card never shows a part lit for the other one. Null when there is
   * nothing to take.
   */
  capture(): Promise<Partial<Record<"dark" | "light", string>> | null>;
  /** Where the camera is: its position and what it looks at. */
  camera(): { position: number[]; target: number[] } | null;
  /** Puts the keyboard focus on the view. */
  focus(): void;
};

export type ViewportProps = {
  /** What to draw, or null when there is nothing. */
  scene: SceneDocument | null;
  /** Scenes with the same key share the camera; another key starts from the corner. */
  cameraKey: string;
  theme: "dark" | "light";
  /** Names of parts that are not drawn. */
  hidden?: readonly string[];
  clip?: Clip | null;
  /** Draws the parts as a ghost at 30 %: what a print has not started to make. */
  ghost?: boolean;
  /** The face or edge the maker picked, drawn in the accent; null for none. */
  marked?: Picked | null;
  /** What Hotend points at (see `ViewMark`). A new spot turns the camera to its side of the part. */
  marks?: readonly ViewMark[];
  /**
   * Changes when Hotend has written a new model: the next scene is fitted again, from
   * the direction the maker left, because it may be another shape. A size the maker
   * changed keeps the camera as it is.
   */
  fresh?: string | number;
  /** The plate's side in millimetres when the scene is a plate: the grid is the plate, in 32 mm cells. */
  plate?: number | null;
  /** A click on a part, or null for a click on nothing. Dragging turns the view instead. */
  onPick?: (picked: Picked | null) => void;
  /** The accessible name of the view. */
  label: string;
  /** Single keys while the view has the focus (DESIGN_SPEC 6.3). */
  onKey?: (event: React.KeyboardEvent) => void;
  ref?: Ref<ViewportHandle>;
};

type Scene = {
  viewer: Viewer;
  document: SceneDocument;
  theme: "dark" | "light";
  tints: readonly string[];
  edge: number | null;
  accent: string;
  fit(): void;
  /** Set when the viewer has been taken down: nothing is drawn with it any more. */
  gone?: boolean;
};

// The parts of the viewer this file reaches into. The viewer's own declarations name
// them, but as types of three.js, which the window does not depend on; these are the few
// members used, as the installed three-cad-viewer 5.1.3 has them (dist/core/viewer.d.ts,
// scene/grid.d.ts, scene/clipping.d.ts, rendering/highlight.d.ts).
type Vec = {
  x: number;
  y: number;
  z: number;
  set(x: number, y: number, z: number): Vec;
  clone(): Vec;
  sub(other: Vec): Vec;
  project(camera: unknown): Vec;
  unproject(camera: unknown): Vec;
};
type Tint = {
  set(value: number): Tint;
  getHex(): number;
  getHSL(target: { h: number; s: number; l: number }): { h: number; s: number; l: number };
  setHSL(h: number, s: number, l: number): Tint;
};
type Light = { intensity: number; position: Vec; clone(): Light };
type GridLine = { material: { color: Tint }; computeLineDistances(): void };
type GridPlane = { children: [{ children: GridLine[] }, ...unknown[]]; remove(...objects: unknown[]): void };
type Component = { id: number; path: string; solidPath: string | null };
type Highlight = {
  registry: { get(id: number): Component | undefined; entries(): Iterable<Component> };
  uniforms: { uHighlightSelectedColor: { value: Tint }; uHighlightHoverColor: { value: Tint } };
  setHover(id: number | null): void;
  setSelected(id: number, flag: boolean): void;
  clear(): void;
};
type Inner = {
  scene: { add(object: unknown): void };
  ambientLight: Light;
  directLight: Light;
  camera: {
    getCamera(): { updateMatrixWorld(): void; quaternion: { x: number; y: number; z: number; w: number } };
    getPosition(): Vec;
  };
  controls: { getTarget(): Vec };
  gridHelper: {
    children: GridPlane[];
    size: number;
    ticks: number;
    colors: Record<string, string[]>;
    showLabels(flag: boolean): void;
    create(nice?: boolean): Promise<void>;
    update(zoom: number, force?: boolean): Promise<void>;
  };
  clipping: {
    setObjectColorCaps(flag: boolean): void;
    _planeMeshGroup?: { children: { material: { color: Tint } }[] } | null;
    _hatchUniforms?: { uHatchOn: { value: number } };
  };
  orientationMarker: {
    setVisible(flag: boolean): void;
    cones?: { material: { color: Tint } }[];
    labels?: { material: { color: Tint } }[];
    axes?: { geometry: { setColors(colours: Float32Array): void } } | null;
  };
  nestedGroup: {
    highlight?: Highlight | null;
    groups?: Record<string, { front?: { material?: { color?: Tint } } | null }>;
    setEdgeColor(colour: number): void;
  };
};

function inner(viewer: Viewer): Inner {
  return viewer.rendered as unknown as Inner;
}
type Camera = {
  position: Vector3Tuple;
  quaternion: QuaternionTuple;
  target: Vector3Tuple;
  zoom: number;
};

/** Every single-key action of the viewer, bound to nothing: Hotend has its own keys. */
const NO_KEYS = Object.fromEntries(
  [
    ["axes", "axes0", "grid", "gridxy", "perspective", "transparent", "blackedges"],
    ["zscale", "reset", "resize", "iso", "front", "rear", "top", "bottom", "left", "right"],
    ["explode", "distance", "properties", "select", "help", "play", "stop"],
    ["tree", "clip", "material", "zebra", "studio"],
  ]
    .flat()
    .map((action) => [action, ""]),
);

function token(element: Element, name: string): string {
  return getComputedStyle(element).getPropertyValue(name).trim();
}

/** The colours of the parts and of their edges, as the theme on the root element has them. */
function stageColours(root: Element): { tints: string[]; edge: number | null } {
  const tints = Array.from({ length: 8 }, (_, index) => token(root, `--hotend-part-${index + 1}`)).filter(
    (tint) => tint !== "",
  );
  return { tints, edge: colourNumber(token(root, "--hotend-stage-edge")) };
}

/** The same, for the theme that is not on screen: read between two frames, never painted. */
function otherColours(theme: "dark" | "light"): { theme: "dark" | "light"; tints: string[]; edge: number | null } {
  const root = document.documentElement;
  const shown = root.dataset.theme;
  const other = theme === "dark" ? "light" : "dark";
  root.dataset.theme = other;
  try {
    return { theme: other, ...stageColours(root) };
  } finally {
    if (shown === undefined) delete root.dataset.theme;
    else root.dataset.theme = shown;
  }
}

function webglWorks(): boolean {
  try {
    return document.createElement("canvas").getContext("webgl2") !== null;
  } catch {
    return false;
  }
}

function toPicked(scene: Scene, x: number, y: number): Picked | null {
  const hit = scene.viewer.idPicker?.pickAt(x, y);
  if (!hit || hit.info.topo === "solid") return null;
  const part = partOfPath(scene.document as MeshDocument, hit.info.solidPath ?? hit.info.path);
  if (part === null) return null;
  return {
    part,
    kind: hit.info.topo,
    path: hit.info.path,
    point: hit.point ? [hit.point.x, hit.point.y, hit.point.z] : null,
  };
}

function applyHidden(scene: Scene, hidden: readonly string[]) {
  for (const part of namedParts(scene.document as MeshDocument)) {
    const visible = !hidden.includes(part.name);
    scene.viewer.setState(part.path, visible ? [1, 1] : [0, 0]);
  }
}

const NORMALS: Record<Clip["axis"], Vector3Tuple> = { x: [-1, 0, 0], y: [0, -1, 0], z: [0, 0, -1] };
const AXES = ["x", "y", "z"] as const;

// One clip plane cuts; the other two are parked far outside the scene. The viewer only
// clips in its "clip" mode, which also draws the cut faces in the part's own colour.
function applyClip(scene: Scene, clip: Clip | null) {
  const { viewer } = scene;
  if (clip === null) {
    // The viewer's own tab switch turns local clipping on and off; it runs only when the
    // change is announced.
    if (viewer.state.get("activeTab") === "clip") viewer.setActiveTab("tree", true);
    return;
  }
  if (viewer.state.get("activeTab") !== "clip") {
    viewer.setActiveTab("clip", true);
    viewer.setClipIntersection(false, false);
    viewer.setClipObjectColorCaps(true, false);
    viewer.setClipPlaneHelpers(false, false);
    // A cut face is the part's own colour, a step darker, and plain: the viewer's
    // section hatching reads as stripes on a part this small.
    const { clipping } = inner(viewer);
    if (clipping._hatchUniforms) clipping._hatchUniforms.uHatchOn.value = 0;
    clipping.setObjectColorCaps(true);
  }
  // The viewer counts a plane's position from the centre of the scene's bounding box.
  const centre = viewer.bbox?.center() ?? [0, 0, 0];
  AXES.forEach((axis, index) => {
    const value = axis === clip.axis ? clip.at - (centre[index] ?? 0) : 100_000;
    viewer.setClipNormal(index as 0 | 1 | 2, NORMALS[axis], value, false);
  });
  viewer.update(true, false);
}

type Bounds = { xmin: number; xmax: number; ymin: number; ymax: number; zmin: number; zmax: number };
type Inset = { top: number; right: number; bottom: number; left: number };

/**
 * What Hotend's furniture covers of a stage that shows the plate (DESIGN_SPEC 10.4),
 * and 24 px more: the plate's border is whole at every size of the window.
 */
const PLATE_INSET: Inset = { top: 80, right: 24, bottom: 96, left: 24 };
/** The same for the part in Design: the verdict bar above, the chips and the size below. */
const DESIGN_INSET: Inset = { top: 104, right: 24, bottom: 72, left: 24 };
/** The plate is seen from above its front left corner, steeply enough that no piece hides another. */
const PLATE_ELEVATION = (55 * Math.PI) / 180;
/** The side of one cell of the plate's grid, in millimetres. */
const PLATE_CELL = 32;

/**
 * The grid as Hotend draws it: the bed, quietly. Every line is the same dashed line in
 * the plate's colour (the viewer draws its two centre lines solid and in axis colours)
 * and there are no tick numbers. The viewer makes its grid anew on every resize and at
 * some zoom steps, and a new grid has its numbers and solid lines again, so this is done
 * inside the grid's own `create`, never once after the fact.
 */
function quietGrid(viewer: Viewer, root: Element, plate: number | null) {
  const grid = inner(viewer).gridHelper;
  const line = colourNumber(token(root, "--hotend-stage-plate"));
  const create = grid.create.bind(grid);
  const quiet = () => {
    for (const plane of grid.children) {
      const [lines, ...labels] = plane.children;
      if (labels.length > 0) plane.remove(...labels);
      const [dashed, ...centre] = lines.children;
      if (!dashed) continue;
      if (line !== null) dashed.material.color.set(line);
      for (const axis of centre) {
        axis.material = dashed.material;
        axis.computeLineDistances();
      }
    }
  };
  grid.create = (nice) => {
    if (plate === null) {
      const made = create(nice);
      quiet();
      return made;
    }
    // The plate's grid is the plate: as large as it, in cells of 32 mm at every zoom.
    grid.size = plate;
    grid.ticks = Math.max(1, Math.round(plate / PLATE_CELL / 2));
    const made = create(false);
    quiet();
    return made;
  };
  grid.showLabels = () => undefined;
  // The grid the viewer made before this: made again, quiet. `create` runs to its end
  // inside this call; only the viewer's own label sizing waits a tick.
  void grid.update(viewer.getCameraZoom(), true);
}

/** How many tick numbers the grid carries: none, once it is quiet. */
function gridLabels(viewer: Viewer): number {
  return inner(viewer).gridHelper.children.reduce((sum, plane) => sum + Math.max(plane.children.length - 1, 0), 0);
}

/** Turns the camera about what it looks at, to a direction, at the distance it has. */
function lookFrom(viewer: Viewer, direction: readonly [number, number, number]) {
  const { camera, controls } = inner(viewer);
  const at = camera.getPosition();
  const target = controls.getTarget();
  const distance = Math.hypot(at.x - target.x, at.y - target.y, at.z - target.z);
  const length = Math.hypot(...direction);
  if (!(distance > 0) || !(length > 0)) return;
  const [x, y, z] = direction.map((part) => (part / length) * distance) as [number, number, number];
  viewer.setCameraLocationSettings([target.x + x, target.y + y, target.z + z], null, [target.x, target.y, target.z], null, false);
}

/** The plate's own view: from the corner the viewer starts at, at 55 degrees above the bed. */
function plateView(viewer: Viewer) {
  const { camera, controls } = inner(viewer);
  const at = camera.getPosition();
  const target = controls.getTarget();
  const flat = Math.hypot(at.x - target.x, at.y - target.y);
  if (!(flat > 0)) return;
  const [x, y] = [(at.x - target.x) / flat, (at.y - target.y) / flat];
  lookFrom(viewer, [x * Math.cos(PLATE_ELEVATION), y * Math.cos(PLATE_ELEVATION), Math.sin(PLATE_ELEVATION)]);
}

/**
 * Zooms and moves the camera, without turning it, so that a box of the scene fills the
 * view inside the insets. The same box in the same view always gives the same picture.
 */
function frameBox(viewer: Viewer, box: Bounds, inset: Inset) {
  const { camera, controls } = inner(viewer);
  const width = Math.max(viewer.cadWidth, 1);
  const height = Math.max(viewer.height, 1);
  const room = {
    width: (2 * Math.max(width - inset.left - inset.right, 8)) / width,
    height: (2 * Math.max(height - inset.top - inset.bottom, 8)) / height,
  };
  const middle = { x: (inset.left - inset.right) / width, y: (inset.bottom - inset.top) / height };
  const point = camera.getPosition().clone();
  const eye = camera.getCamera();
  const extent = () => {
    eye.updateMatrixWorld();
    const seen = { left: Infinity, right: -Infinity, bottom: Infinity, top: -Infinity };
    for (const x of [box.xmin, box.xmax]) {
      for (const y of [box.ymin, box.ymax]) {
        for (const z of [box.zmin, box.zmax]) {
          point.set(x, y, z).project(eye);
          seen.left = Math.min(seen.left, point.x);
          seen.right = Math.max(seen.right, point.x);
          seen.bottom = Math.min(seen.bottom, point.y);
          seen.top = Math.max(seen.top, point.y);
        }
      }
    }
    return seen;
  };
  // A perspective view changes a little when the camera moves, so: zoom, move, and again.
  for (let pass = 0; pass < 3; pass += 1) {
    const before = extent();
    const scale = Math.min(room.width / (before.right - before.left), room.height / (before.top - before.bottom));
    if (!Number.isFinite(scale) || scale <= 0) return;
    viewer.setCameraZoom(viewer.getCameraZoom() * scale, false);
    const after = extent();
    const target = controls.getTarget();
    const depth = point.set(target.x, target.y, target.z).project(eye).z;
    const from = point.clone().set((after.left + after.right) / 2, (after.bottom + after.top) / 2, depth).unproject(eye);
    const to = point.clone().set(middle.x, middle.y, depth).unproject(eye);
    const shift = from.sub(to);
    const at = camera.getPosition();
    viewer.setCameraLocationSettings(
      [at.x + shift.x, at.y + shift.y, at.z + shift.z],
      null,
      [target.x + shift.x, target.y + shift.y, target.z + shift.z],
      null,
      false,
    );
  }
}

/** The middle of the view that a project card shows: 3:2, with a margin around the part. */
function cardInset(viewer: Viewer): Inset {
  const width = Math.max(viewer.cadWidth, 1);
  const height = Math.max(viewer.height, 1);
  const cropWidth = Math.min(width, height * 1.5);
  const cropHeight = cropWidth / 1.5;
  const side = (width - cropWidth) / 2 + cropWidth * 0.1;
  const end = (height - cropHeight) / 2 + cropHeight * 0.12;
  return { top: end, right: side, bottom: end, left: side };
}

// Light (DESIGN_SPEC 10.2): the part reads as a solid in its own colour. A soft light
// from everywhere, a little from the camera, and a key light fixed above the front left
// of the bed. From the corner view the wall that faces the maker is its tint as the token
// gives it (4 % under), the top is a step lighter and the other wall a step darker, each
// by a tenth or more. The numbers are shares of full light before the screen's gamma;
// three.js counts light in multiples of pi.
const AMBIENT = 0.623;
const HEADLIGHT = 0.1;
const KEY = 0.51;
const KEY_FROM = [-0.6, -1, 1.854] as const;

/**
 * What the window changes of a freshly rendered viewer: the light, the grid without
 * tick labels and coloured axis lines, the axis marker drawn on every frame, plain cut
 * faces, and hover and pick colours that belong to the theme.
 */
function dress(scene: Scene, root: Element, plate: number | null, pickable: () => boolean, painted: () => void) {
  const { viewer } = scene;
  const parts = inner(viewer);

  // Every frame the viewer draws, for whatever reason, is followed by Hotend's own
  // drawing over the view: the axis marker and what Hotend points at.
  const update = viewer.update;
  viewer.update = (_marker, notify) => {
    // A frame asked for after the viewer was taken down (a late event of its controls, a
    // print's progress arriving as the stage changes) draws nothing.
    if (scene.gone) return;
    update(false, notify);
    painted();
  };
  // The viewer's axis marker (cones, its own font, drawn in its own pass) stays off;
  // Hotend draws three quiet lines of its own (DESIGN_SPEC 4.3.2, 10.1).
  parts.orientationMarker.setVisible(false);

  parts.ambientLight.intensity = Math.PI * AMBIENT;
  parts.directLight.intensity = Math.PI * HEADLIGHT;
  const key = parts.directLight.clone();
  key.intensity = Math.PI * KEY;
  key.position.set(...KEY_FROM);
  parts.scene.add(key);

  quietGrid(viewer, root, plate);

  const caps = parts.clipping.setObjectColorCaps;
  parts.clipping.setObjectColorCaps = (flag) => {
    caps(flag);
    if (!flag) return;
    for (const cap of parts.clipping._planeMeshGroup?.children ?? []) {
      const shade = cap.material.color.getHSL({ h: 0, s: 0, l: 0 });
      cap.material.color.setHSL(shade.h, shade.s, shade.l * 0.88);
    }
  };

  // What the pointer is over is tinted towards the accent, in the colour of its own
  // part; nothing is tinted where a click would pick nothing.
  const highlight = parts.nestedGroup.highlight;
  if (highlight) {
    const setHover = highlight.setHover.bind(highlight);
    highlight.setHover = (id) => {
      const component = id === null || !pickable() ? undefined : highlight.registry.get(id);
      if (!component) {
        setHover(null);
        return;
      }
      const tint = mixColours(tintOf(scene, component.solidPath ?? component.path), scene.accent, 0.18);
      if (tint !== null) highlight.uniforms.uHighlightHoverColor.value.set(tint);
      setHover(id);
    };
  }
  viewer.update(true, false);
}

/** The colour of the part that owns a path of the viewer. */
function tintOf(scene: Scene, path: string): string {
  const part = scene.document.shapes.parts.find((candidate) => path === candidate.id || path.startsWith(`${candidate.id}/`));
  const index = part?.tint;
  return index === undefined || scene.tints.length === 0 ? "#808080" : (scene.tints[index % scene.tints.length] ?? "#808080");
}

/** Draws the picked face or edge in the accent, mixed 35 % into its part's own colour. */
function applyMarked(scene: Scene, marked: Picked | null) {
  const highlight = inner(scene.viewer).nestedGroup.highlight;
  if (!highlight) return;
  highlight.clear();
  if (marked !== null) {
    for (const component of highlight.registry.entries()) {
      if (component.path !== marked.path) continue;
      const tint = mixColours(tintOf(scene, component.solidPath ?? component.path), scene.accent, 0.35);
      if (tint !== null) highlight.uniforms.uHighlightSelectedColor.value.set(tint);
      highlight.setSelected(component.id, true);
      break;
    }
  }
  scene.viewer.update(true, false);
}

function applyGhost(scene: Scene, ghost: boolean) {
  if (scene.viewer.getTransparent() === ghost) return;
  if (ghost) scene.viewer.setOpacity(0.3, false);
  scene.viewer.setTransparent(ghost, false);
}

/** Where a point of the model is on the view, in CSS pixels from its top left corner. */
function onView(viewer: Viewer, point: readonly [number, number, number]): { x: number; y: number } {
  const { camera } = inner(viewer);
  const eye = camera.getCamera();
  eye.updateMatrixWorld();
  const seen = camera.getPosition().clone().set(point[0], point[1], point[2]).project(eye);
  return { x: ((seen.x + 1) / 2) * viewer.cadWidth, y: ((1 - seen.y) / 2) * viewer.height };
}

/** A direction of the model as the camera sees it: x to the right, y up, z towards the eye. */
function seenAxis(viewer: Viewer, axis: readonly [number, number, number]): [number, number, number] {
  // The camera's rotation, inverted, applied to the axis: v' = q* v q.
  const { x, y, z, w } = inner(viewer).camera.getCamera().quaternion;
  const [qx, qy, qz] = [-x, -y, -z];
  const [vx, vy, vz] = axis;
  const [tx, ty, tz] = [2 * (qy * vz - qz * vy), 2 * (qz * vx - qx * vz), 2 * (qx * vy - qy * vx)];
  return [vx + w * tx + (qy * tz - qz * ty), vy + w * ty + (qz * tx - qx * tz), vz + w * tz + (qx * ty - qy * tx)];
}

const COMPASS = 48;
const COMPASS_ARM = 15;
const AXIS_NAMES = ["X", "Y", "Z"] as const;
const UNIT: readonly (readonly [number, number, number])[] = [
  [1, 0, 0],
  [0, 1, 0],
  [0, 0, 1],
];

/** Draws Hotend's axis marker and marks for the camera as it is now. */
/** The convex outline of points on the view (Andrew's monotone chain). */
function hull(points: readonly { x: number; y: number }[]): { x: number; y: number }[] {
  const sorted = [...points].sort((a, b) => a.x - b.x || a.y - b.y);
  const cross = (o: { x: number; y: number }, a: { x: number; y: number }, b: { x: number; y: number }) =>
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
  const half = (list: readonly { x: number; y: number }[]) => {
    const out: { x: number; y: number }[] = [];
    for (const point of list) {
      while (out.length >= 2 && cross(out[out.length - 2] ?? point, out[out.length - 1] ?? point, point) <= 0) out.pop();
      out.push(point);
    }
    out.pop();
    return out;
  };
  return [...half(sorted), ...half([...sorted].reverse())];
}

/** The plate's surface, as the camera sees it: one quiet quadrilateral at Z 0. */
function paintPlate(viewer: Viewer, under: SVGSVGElement | null, plate: number | null) {
  const surface = under?.querySelector("polygon");
  if (!surface || plate === null) return;
  const half = plate / 2;
  const corners = ([[-half, -half], [half, -half], [half, half], [-half, half]] as const).map(([x, y]) => onView(viewer, [x, y, 0]));
  surface.setAttribute("points", corners.map((point) => `${point.x.toFixed(1)},${point.y.toFixed(1)}`).join(" "));
}

function paintOver(viewer: Viewer, compass: SVGSVGElement | null, overlay: SVGSVGElement | null, marks: readonly ViewMark[]) {
  if (compass) {
    const centre = COMPASS / 2;
    AXIS_NAMES.forEach((name, index) => {
      const [x, y] = seenAxis(viewer, UNIT[index] ?? [0, 0, 1]);
      const end = { x: centre + x * COMPASS_ARM, y: centre - y * COMPASS_ARM };
      const line = compass.querySelector(`[data-axis="${name}"]`);
      line?.setAttribute("x2", end.x.toFixed(1));
      line?.setAttribute("y2", end.y.toFixed(1));
      const label = compass.querySelector(`[data-axis-label="${name}"]`);
      label?.setAttribute("x", (centre + x * (COMPASS_ARM + 6)).toFixed(1));
      label?.setAttribute("y", (centre - y * (COMPASS_ARM + 6)).toFixed(1));
    });
  }
  if (!overlay) return;
  const drawn = overlay.querySelectorAll<SVGGElement>("[data-mark]");
  marks.forEach((mark, index) => {
    const group = drawn[index];
    if (!group) return;
    if (mark.kind === "spot") {
      const at = onView(viewer, mark.at);
      group.setAttribute("transform", `translate(${at.x.toFixed(1)} ${at.y.toFixed(1)})`);
      group.dataset.x = at.x.toFixed(0);
      group.dataset.y = at.y.toFixed(0);
      return;
    }
    if (mark.kind === "box") {
      const corners = [0, 1, 2, 3, 4, 5, 6, 7].map((n) =>
        onView(viewer, [(n & 1 ? mark.max : mark.min)[0], (n & 2 ? mark.max : mark.min)[1], (n & 4 ? mark.max : mark.min)[2]]),
      );
      group.querySelector("polygon")?.setAttribute(
        "points",
        hull(corners)
          .map((point) => `${point.x.toFixed(1)},${point.y.toFixed(1)}`)
          .join(" "),
      );
      return;
    }
    const [a, b] = [onView(viewer, mark.from), onView(viewer, mark.to)];
    const length = Math.hypot(b.x - a.x, b.y - a.y) || 1;
    // The end ticks stand across the line, 5 px to each side.
    const across = { x: (-(b.y - a.y) / length) * 5, y: ((b.x - a.x) / length) * 5 };
    const set = (name: string, from: { x: number; y: number }, to: { x: number; y: number }) => {
      const line = group.querySelector(`[data-line="${name}"]`);
      line?.setAttribute("x1", from.x.toFixed(1));
      line?.setAttribute("y1", from.y.toFixed(1));
      line?.setAttribute("x2", to.x.toFixed(1));
      line?.setAttribute("y2", to.y.toFixed(1));
    };
    set("main", a, b);
    set("start", { x: a.x - across.x, y: a.y - across.y }, { x: a.x + across.x, y: a.y + across.y });
    set("end", { x: b.x - across.x, y: b.y - across.y }, { x: b.x + across.x, y: b.y + across.y });
    const label = group.querySelector("text");
    // The number sits beside the middle of the line, on the side that is lower on screen.
    const side = across.y >= 0 ? 1 : -1;
    label?.setAttribute("x", ((a.x + b.x) / 2 + across.x * side * 2.6).toFixed(1));
    label?.setAttribute("y", ((a.y + b.y) / 2 + across.y * side * 2.6).toFixed(1));
    group.dataset.length = length.toFixed(0);
  });
}

const NOTHING_HIDDEN: readonly string[] = [];
const NO_MARKS: readonly ViewMark[] = [];

export function Viewport({
  scene: sceneDocument,
  cameraKey,
  theme,
  hidden = NOTHING_HIDDEN,
  clip = null,
  ghost = false,
  marked = null,
  marks = NO_MARKS,
  fresh,
  plate = null,
  onPick,
  label,
  onKey,
  ref,
}: ViewportProps) {
  const host = useRef<HTMLDivElement>(null);
  const frame = useRef<HTMLDivElement>(null);
  const compass = useRef<SVGSVGElement>(null);
  const overlay = useRef<SVGSVGElement>(null);
  const under = useRef<SVGSVGElement>(null);
  const scene = useRef<Scene | null>(null);
  const latest = useRef({ onPick, hidden, clip, ghost, marked, marks, fresh });
  // True once the maker has moved the camera of a kind of scene (a drag, the wheel):
  // from then on the framing is theirs. Until then, and again after Fit, the view is
  // fitted whenever the stage changes its size or gets another part.
  const touched = useRef(new Map<string, boolean>());
  // The `fresh` each kind of scene was last fitted for.
  const fitted = useRef(new Map<string, string | number | undefined>());
  // Where the camera was when the last viewer of each kind of scene was taken down. A new
  // version or another theme makes a new viewer; the camera stays where the maker left it
  // (DESIGN_SPEC 10.2).
  const cameras = useRef(new Map<string, Camera>());
  const [webgl] = useState(webglWorks);
  // Without WebGL the stage says so in its own words (Stage.tsx); nothing is drawn here.

  useEffect(() => {
    latest.current = { onPick, hidden, clip, ghost, marked, marks, fresh };
  }, [onPick, hidden, clip, ghost, marked, marks, fresh]);

  // One viewer per scene and theme. The viewer owns everything inside `host`.
  useEffect(() => {
    const container = host.current;
    if (!container || sceneDocument === null || !webgl) return;
    const kept = cameras.current;

    const width = Math.max(container.clientWidth, 1);
    const height = Math.max(container.clientHeight, 1);
    // The tokens of the theme are read from the root element, where the theme is set. The
    // viewer writes its own `data-theme` on its container and on <body>, and both keep the
    // last viewer's value until the next one is drawn.
    const root = document.documentElement;
    const { tints, edge } = stageColours(root);
    const onPlate = cameraKey === "plate";

    // The file as the engine wrote it, with only the parts' colours replaced. The viewer
    // writes names and colours into the page as HTML: what it is given is checked here
    // once more, whatever made the document (./mesh.ts).
    const drawn = coloured(sceneDocument, tints, (name) => token(root, name));
    checkScene(drawn);

    const display = new Display(container, {
      cadWidth: width,
      height,
      treeWidth: 0,
      theme,
      tools: false,
      glass: false,
      pinning: false,
      measureTools: false,
      selectTool: false,
      explodeTool: false,
      zscaleTool: false,
      zebraTool: false,
      studioTool: false,
      externalMeasurementBackend: false,
    });
    const viewer = new Viewer(
      display,
      {
        cadWidth: width,
        height,
        treeWidth: 0,
        theme,
        tools: false,
        glass: false,
        pinning: false,
        keymap: NO_KEYS,
      },
      () => undefined,
    );
    viewer.render(
      drawn as unknown as Shapes,
      {
        metalness: 0,
        roughness: 0.85,
        ...(edge === null ? {} : { edgeColor: edge }),
      },
      {
        control: "orbit",
        up: "Z",
        ortho: false,
        grid: [true, false, false],
        axes: false,
        // The grid is the bed: at Z 0 around the origin, not under the scene's bounding
        // box, where the viewer would otherwise put it.
        axes0: true,
        centerGrid: true,
        ...(onPlate ? {} : (kept.get(cameraKey) ?? {})),
      },
    );
    const bounds = sceneDocument.shapes.bb as Bounds | undefined;
    const current: Scene = {
      viewer,
      document: sceneDocument,
      theme,
      tints,
      edge,
      accent: token(root, "--app-accent"),
      // The plate always fills the stage the same way. A part fills the room Hotend's
      // furniture leaves, from the direction the camera has (10.2).
      fit() {
        if (!bounds) return;
        if (onPlate) frameBox(viewer, bounds, PLATE_INSET);
        else frameBox(viewer, bounds, DESIGN_INSET);
        // For tests: the size of the view the scene was last framed for.
        container.dataset.framed = `${viewer.cadWidth}x${viewer.height}`;
      },
    };
    dress(
      current,
      root,
      onPlate ? plate : null,
      () => latest.current.onPick !== undefined,
      () => {
        paintOver(viewer, compass.current, overlay.current, latest.current.marks);
        paintPlate(viewer, under.current, onPlate ? plate : null);
        // For tests: how many tick numbers the grid carried in the frame just drawn.
        container.dataset.ticks = String(gridLabels(viewer));
      },
    );
    if (onPlate) plateView(viewer);
    // The camera is the maker's once they have moved it, and only for the model they
    // moved it around: a model Hotend has written anew is fitted again.
    const wasFresh = fitted.current.get(cameraKey);
    if (onPlate || !kept.has(cameraKey) || touched.current.get(cameraKey) !== true || wasFresh !== latest.current.fresh) {
      current.fit();
      touched.current.set(cameraKey, false);
    }
    fitted.current.set(cameraKey, latest.current.fresh);
    scene.current = current;
    applyHidden(current, latest.current.hidden);
    applyGhost(current, latest.current.ghost);
    applyClip(current, latest.current.clip);
    applyMarked(current, latest.current.marked);
    // The viewer's own element takes the keyboard focus; the view has one tab stop, the
    // frame around it, which also carries its name.
    for (const element of [container, ...container.querySelectorAll("[tabindex]")]) element.removeAttribute("tabindex");
    // For tests and for anything that waits for the part: how many parts are on the stage.
    container.dataset.parts = String(namedParts(sceneDocument as MeshDocument).length);

    // A column that opens or closes changes the stage on every frame for a quarter of a
    // second: the viewer is resized at the first change, so that a part that arrives while
    // a column is still opening is not left framed for a stage that no longer exists, and
    // once more when the size has stopped changing.
    let settle: ReturnType<typeof setTimeout> | null = null;
    const fitStage = () => {
      const next = [Math.max(container.clientWidth, 1), Math.max(container.clientHeight, 1)];
      if (next[0] === viewer.cadWidth && next[1] === viewer.height) return;
      viewer.resizeCadView(next[0] ?? 1, 0, next[1] ?? 1, false);
      // A stage that changed its size is framed again, unless the framing is the maker's.
      if (onPlate || touched.current.get(cameraKey) !== true) current.fit();
      // The viewer drew its frame before its grid was quiet again: drawn once more.
      viewer.update(true, false);
      container.dataset.resized = String(Number(container.dataset.resized ?? 0) + 1);
    };
    const resize = new ResizeObserver(() => {
      if (settle === null) fitStage();
      else clearTimeout(settle);
      settle = setTimeout(() => {
        settle = null;
        fitStage();
      }, 80);
    });
    resize.observe(container);

    return () => {
      resize.disconnect();
      if (settle !== null) clearTimeout(settle);
      scene.current = null;
      current.gone = true;
      delete container.dataset.parts;
      const { position, quaternion, target, zoom } = viewer.getCameraLocationSettings();
      if (!onPlate && position.length === 3 && quaternion.length === 4 && target.length === 3) {
        kept.set(cameraKey, {
          position: position as Vector3Tuple,
          quaternion: quaternion as QuaternionTuple,
          target: target as Vector3Tuple,
          zoom,
        });
      }
      viewer.dispose();
      container.replaceChildren();
      // The viewer's own theme marks (see above). Left on <body>, the last one would
      // decide the colours of every screen that has no viewer.
      container.removeAttribute("data-theme");
      document.body.removeAttribute("data-theme");
    };
  }, [sceneDocument, cameraKey, theme, webgl, plate]);

  // Parts hidden by name: neither their faces nor their edges are drawn.
  useEffect(() => {
    if (scene.current) applyHidden(scene.current, hidden);
  }, [hidden]);

  useEffect(() => {
    if (scene.current) applyClip(scene.current, clip);
  }, [clip]);

  useEffect(() => {
    if (scene.current) applyGhost(scene.current, ghost);
  }, [ghost]);

  useEffect(() => {
    if (scene.current) applyMarked(scene.current, marked);
  }, [marked]);

  // A click that did not drag picks. The viewer's own picking is on double-click and
  // reports to its tree; Hotend asks the viewer's picker directly.
  useEffect(() => {
    const container = host.current;
    if (!container) return;
    let down: { x: number; y: number } | null = null;
    const onDown = (event: PointerEvent) => {
      down = event.button === 0 ? { x: event.clientX, y: event.clientY } : null;
    };
    // A drag or the wheel moves the camera: the framing is the maker's from here on.
    const onMove = (event: PointerEvent) => {
      if (event.buttons === 0 || event.target !== container.querySelector("canvas")) return;
      if (down && Math.hypot(event.clientX - down.x, event.clientY - down.y) <= 4) return;
      touch();
    };
    const touch = () => {
      const key = container.dataset.camera;
      if (key !== undefined) touched.current.set(key, true);
    };
    const onUp = (event: PointerEvent) => {
      const start = down;
      down = null;
      const current = scene.current;
      const canvas = container.querySelector("canvas");
      if (!start || !current || !canvas || event.target !== canvas) return;
      if (Math.hypot(event.clientX - start.x, event.clientY - start.y) > 4) return;
      const box = canvas.getBoundingClientRect();
      latest.current.onPick?.(toPicked(current, event.clientX - box.left, event.clientY - box.top));
      // The pick drew a frame of its own; the stage is drawn again, whole.
      current.viewer.update(true, false);
      // For tests: how many clicks the stage has answered.
      container.dataset.clicks = String(Number(container.dataset.clicks ?? 0) + 1);
    };
    container.addEventListener("pointerdown", onDown);
    container.addEventListener("pointermove", onMove);
    container.addEventListener("pointerup", onUp);
    container.addEventListener("wheel", touch, { passive: true });
    return () => {
      container.removeEventListener("pointerdown", onDown);
      container.removeEventListener("pointermove", onMove);
      container.removeEventListener("pointerup", onUp);
      container.removeEventListener("wheel", touch);
    };
  }, []);

  // What Hotend points at is drawn for the camera as it is; a new place of a fault
  // turns the camera to the side of the part the place is on, so that it can be seen.
  const spot = marks.find((mark) => mark.kind === "spot");
  const spotKey = spot ? spot.at.join(",") : null;
  useEffect(() => {
    scene.current?.viewer.update(true, false);
  }, [marks]);
  useEffect(() => {
    const current = scene.current;
    if (!current) return;
    const bounds = current.document.shapes.bb as Bounds | undefined;
    const at = spotKey?.split(",").map(Number);
    if (at && bounds && host.current?.dataset.camera !== "plate") {
      const centre = [(bounds.xmin + bounds.xmax) / 2, (bounds.ymin + bounds.ymax) / 2];
      const half = [Math.max((bounds.xmax - bounds.xmin) / 2, 1e-6), Math.max((bounds.ymax - bounds.ymin) / 2, 1e-6)];
      // The side wall the place is nearest to, seen from a little to its left and from above.
      const out = [((at[0] ?? 0) - (centre[0] ?? 0)) / (half[0] ?? 1), ((at[1] ?? 0) - (centre[1] ?? 0)) / (half[1] ?? 1)];
      const wall: [number, number] = Math.abs(out[0] ?? 0) > Math.abs(out[1] ?? 0) ? [Math.sign(out[0] ?? 0), 0] : [0, Math.sign(out[1] ?? 0) || -1];
      lookFrom(current.viewer, [wall[0] * 0.8 - wall[1] * 0.35, wall[1] * 0.8 + wall[0] * 0.35, 0.5]);
      current.fit();
      current.viewer.update(true, false);
    }
  }, [spotKey]);

  useImperativeHandle(
    ref,
    () => ({
      fit() {
        const current = scene.current;
        if (!current) return;
        current.fit();
        current.viewer.update(true, false);
        // A fitted view is fitted again when the stage changes its size.
        const key = host.current?.dataset.camera;
        if (key !== undefined) touched.current.set(key, false);
      },
      view(direction) {
        const current = scene.current;
        if (!current) return;
        current.viewer.presetCamera(direction, null, false);
        current.fit();
        current.viewer.update(true, false);
        const key = host.current?.dataset.camera;
        if (key !== undefined) touched.current.set(key, false);
      },
      pickAt(x, y) {
        return scene.current ? toPicked(scene.current, x, y) : null;
      },
      capture() {
        const current = scene.current;
        const canvas = host.current?.querySelector("canvas");
        const bounds = current?.document.shapes.bb as Bounds | undefined;
        if (!current || !canvas || !bounds) return Promise.resolve(null);
        const { viewer } = current;
        const parts = inner(viewer);
        const before = viewer.getCameraLocationSettings();
        const grids = viewer.getGrids();
        // All in one go, between two frames: the maker never sees the card's view.
        const pictures: Partial<Record<"dark" | "light", string>> = {};
        const repainted: [Tint, number][] = [];
        try {
          parts.nestedGroup.highlight?.clear();
          viewer.setGrids([false, false, false], false);
          viewer.presetCamera("iso", null, false);
          frameBox(viewer, bounds, cardInset(viewer));
          pictures[current.theme] = canvas.toDataURL("image/png");
          // The same view in the other theme's colours: the faces and edges are painted
          // over, drawn once, and painted back.
          const other = otherColours(current.theme);
          for (const [path, group] of Object.entries(parts.nestedGroup.groups ?? {})) {
            const colour = group.front?.material?.color;
            const tint = colourNumber(tintOf({ ...current, tints: other.tints }, path));
            if (!colour || tint === null) continue;
            repainted.push([colour, colour.getHex()]);
            colour.set(tint);
          }
          if (repainted.length > 0) {
            if (other.edge !== null) parts.nestedGroup.setEdgeColor(other.edge);
            viewer.update(true, false);
            pictures[other.theme] = canvas.toDataURL("image/png");
          }
          return Promise.resolve(pictures);
        } catch {
          return Promise.resolve(Object.keys(pictures).length > 0 ? pictures : null);
        } finally {
          for (const [colour, was] of repainted) colour.set(was);
          if (repainted.length > 0 && current.edge !== null) parts.nestedGroup.setEdgeColor(current.edge);
          viewer.setGrids(grids, false);
          viewer.setCameraLocationSettings(
            before.position as Vector3Tuple,
            before.quaternion as QuaternionTuple,
            before.target as Vector3Tuple,
            before.zoom,
            false,
          );
          applyMarked(current, latest.current.marked);
        }
      },
      camera() {
        const current = scene.current;
        if (!current) return null;
        const { position, target } = current.viewer.getCameraLocationSettings();
        return { position: [...position], target: [...target] };
      },
      focus() {
        frame.current?.focus();
      },
    }),
    [],
  );

  return (
    <div
      ref={frame}
      // The ring of the focus is drawn inside the frame: outside it the stage's
      // neighbours cut it to two stray lines.
      className="hotend-viewport app-ring-inset"
      data-testid="viewport"
      tabIndex={0}
      role="application"
      aria-label={label}
      onKeyDown={onKey}
    >
      {/* The plate's surface, under the transparent canvas: the parts lie on something. */}
      {cameraKey === "plate" && plate !== null && sceneDocument !== null && webgl ? (
        <svg ref={under} className="hotend-viewport-overlay" aria-hidden="true">
          <polygon className="hotend-plate-surface" data-testid="plate-surface" />
        </svg>
      ) : null}
      {/* The drawing itself says nothing to a screen reader: the frame carries the name. */}
      <div ref={host} className="hotend-viewport-host" data-testid="viewport-host" data-camera={cameraKey} aria-hidden="true" />
      <svg ref={overlay} className="hotend-viewport-overlay" data-testid="view-marks" aria-hidden="true">
        {marks.map((mark, index) =>
              mark.kind === "box" ? (
                <g key={index} data-mark="box">
                  <polygon className="hotend-mark-box" />
                </g>
              ) : mark.kind === "spot" ? (
                <g key={index} data-mark="spot">
                  <circle r="6" className="hotend-mark-ring" />
                  {mark.label ? (
                    <text x="12" y="4" className="hotend-mark-label">
                      {mark.label}
                    </text>
                  ) : null}
                </g>
              ) : (
                <g key={index} data-mark="span">
                  <line data-line="main" className="hotend-mark-line" />
                  <line data-line="start" className="hotend-mark-line" />
                  <line data-line="end" className="hotend-mark-line" />
                  <text className="hotend-mark-label" textAnchor="middle" dominantBaseline="middle">
                    {mark.label}
                  </text>
                </g>
              ),
            )}
      </svg>
      {/* Which way the model's axes point: three quiet lines, no cones, the window's own face. */}
      {sceneDocument !== null && webgl ? (
        <svg
          ref={compass}
          className="hotend-viewport-compass"
          width={COMPASS}
          height={COMPASS}
          viewBox={`0 0 ${COMPASS} ${COMPASS}`}
          data-testid="axis-marker"
          aria-hidden="true"
        >
          {AXIS_NAMES.map((name) => (
            <line key={name} data-axis={name} x1={COMPASS / 2} y1={COMPASS / 2} x2={COMPASS / 2} y2={COMPASS / 2} />
          ))}
          {AXIS_NAMES.map((name) => (
            <text key={name} data-axis-label={name} textAnchor="middle" dominantBaseline="central">
              {name}
            </text>
          ))}
        </svg>
      ) : null}
    </div>
  );
}

/** True when this computer can draw 3D in the window. */
export function canShow3d(): boolean {
  return webglWorks();
}
