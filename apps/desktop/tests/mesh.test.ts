import { describe, expect, it } from "vitest";

import { checkScene, colourNumber, namedParts, parseMesh, partOfPath } from "@/hotend/viewport/mesh";
import { coloured, designScene, plateOutline, plateScene, quaternionOf } from "@/hotend/viewport/scene";

const MESH = JSON.stringify({
  instances: [{}, {}],
  shapes: {
    version: 3,
    name: "model",
    id: "/model",
    parts: [
      { id: "/model/box", name: "box", color: "#4c78a8", shape: { ref: 0 } },
      { id: "/model/lid", name: "lid", color: "#f58518", shape: { ref: 1 } },
      { id: "/model/box_2", name: "box_2", color: "#000000", shape: { ref: 0 } },
    ],
  },
  hotend: {
    protocol: 0,
    parts: { box: "/model/box", lid: "/model/lid", box_2: "/model/box_2" },
  },
});

describe("mesh.json", () => {
  it("lists the model's parts in drawing order, by the names of the hotend map", () => {
    expect(namedParts(parseMesh(MESH))).toEqual([
      { name: "box", path: "/model/box" },
      { name: "lid", path: "/model/lid" },
      { name: "box_2", path: "/model/box_2" },
    ]);
  });

  it("traces a picked face or edge back to its part", () => {
    const mesh = parseMesh(MESH);
    expect(partOfPath(mesh, "/model/box/faces/faces_3")).toBe("box");
    expect(partOfPath(mesh, "/model/lid/edges/edges_12")).toBe("lid");
    expect(partOfPath(mesh, "/model/lid")).toBe("lid");
    // A path that only starts with the same letters is another part.
    expect(partOfPath(mesh, "/model/box_2/faces/faces_0")).toBe("box_2");
    expect(partOfPath(mesh, "/model/boxer/faces/faces_0")).toBeNull();
    expect(partOfPath(mesh, "/other/box/faces/faces_0")).toBeNull();
  });

  it("replaces only the colours, repeating the tints, and leaves the input alone", () => {
    const mesh = parseMesh(MESH);
    const result = coloured(designScene(mesh), ["#111111", "#222222"], () => "");
    expect(result.shapes.parts.map((part) => part.color)).toEqual(["#111111", "#222222", "#111111"]);
    expect(result.shapes.parts.map((part) => part.shape)).toEqual(mesh.shapes.parts.map((part) => part.shape));
    expect(result.shapes.parts.every((part) => !("tint" in part))).toBe(true);
    expect(result.instances).toBe(mesh.instances);
    expect(mesh.shapes.parts[0]?.color).toBe("#4c78a8");
    expect(coloured(designScene(mesh), [], () => "").shapes.parts[0]?.color).toBe("#4c78a8");
  });

  it("lays the pieces of a plate out around the plate's centre, sharing each part's mesh", () => {
    const mesh = parseMesh(MESH);
    const identity = [1, 0, 0, 30, 0, 1, 0, 40, 0, 0, 1, 0];
    // Upside down: a half turn about X, then lifted by 10.
    const flipped = [1, 0, 0, 100, 0, -1, 0, 60, 0, 0, -1, 10];
    const scene = plateScene(mesh, [256, 256], [
      { name: "box", part: "box", transform: identity },
      { name: "lid", part: "lid", transform: flipped },
      { name: "lid.2", part: "lid", transform: identity },
    ]);
    const [plate, box, lid, lid2] = scene.shapes.parts;
    expect(plate?.type).toBe("edges");
    expect(box?.loc).toEqual([[30 - 128, 40 - 128, 0], [0, 0, 0, 1]]);
    expect(lid?.loc).toEqual([[100 - 128, 60 - 128, 10], [1, 0, 0, 0]]);
    expect(lid?.shape).toEqual(lid2?.shape);
    expect((scene.hotend as { parts: unknown }).parts).toEqual({ box: box?.id, lid: lid?.id, "lid.2": lid2?.id });
    expect(coloured(scene, ["#111111", "#222222"], () => "#abcdef").shapes.parts.map((part) => part.color)).toEqual([
      "#abcdef",
      "#111111",
      "#222222",
      "#222222",
    ]);
    // The empty plate: only the outline, drawn around the origin.
    const empty = plateScene(null, [256, 256], []);
    expect(empty.shapes.parts).toHaveLength(1);
    expect(JSON.stringify(plateOutline([256, 256]).shape)).toContain("-128");
  });

  it("turns a rotation matrix into the quaternion the viewer takes", () => {
    expect(quaternionOf([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0])).toEqual([0, 0, 0, 1]);
    const quarter = quaternionOf([0, -1, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0]);
    expect(quarter[2]).toBeCloseTo(Math.SQRT1_2);
    expect(quarter[3]).toBeCloseTo(Math.SQRT1_2);
  });

  it("refuses what is not a mesh of this protocol", () => {
    expect(() => parseMesh("{}")).toThrow();
    expect(() => parseMesh("not json")).toThrow();
    expect(() =>
      parseMesh(JSON.stringify({ shapes: { parts: [] }, hotend: { protocol: 1, parts: {} } })),
    ).toThrow(/protocol 1/);
    expect(() => parseMesh(JSON.stringify({ shapes: {}, hotend: { protocol: 0, parts: {} } }))).toThrow();
  });

  // three-cad-viewer writes a leaf's name and colour into the page as HTML and builds
  // selectors from the names, also for the tree it never shows.
  describe("read as hostile input", () => {
    const base = JSON.parse(MESH) as {
      shapes: { name: string; id: string; parts: Record<string, unknown>[]; [field: string]: unknown };
      hotend: { protocol: number; parts: Record<string, string>; [field: string]: unknown };
    };
    const changed = (change: (mesh: typeof base) => void): string => {
      const mesh = structuredClone(base);
      change(mesh);
      return JSON.stringify(mesh);
    };
    const part = (mesh: typeof base): Record<string, unknown> => mesh.shapes.parts[0] ?? {};
    const IMG = "box<img src=x onerror=alert(1)>";

    it("refuses a name that is not a part's or a ghost's", () => {
      for (const name of [IMG, 'box"]', "Box", "box 2", "", "ghost.", "ghost.Ref", "hotend-plate", "piece-1-box", "a".repeat(33)]) {
        expect(() => parseMesh(changed((mesh) => {
          part(mesh).name = name;
          part(mesh).id = `/model/${name}`;
        })), name).toThrow();
      }
      expect(() => parseMesh(changed((mesh) => {
        part(mesh).name = 7;
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        mesh.shapes.name = IMG;
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        mesh.shapes.id = `/${IMG}`;
      }))).toThrow();
    });

    it("refuses a path that is not the name under /model, two leaves of one name, and a tree below a part", () => {
      expect(() => parseMesh(changed((mesh) => {
        part(mesh).id = `/model/${IMG}`;
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        part(mesh).id = "/model/lid";
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        mesh.shapes.parts.push({ ...part(mesh) });
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        part(mesh).parts = [{ id: "/model/box/inner", name: IMG }];
      }))).toThrow();
    });

    it("refuses a colour that is not #RRGGBB and any other text that is not one plain word", () => {
      for (const colour of ['red"><img src=x>', "red", "#fff", "#4c78a8;background:url(x)", 5, ["#4c78a8", "<b>"]]) {
        expect(() => parseMesh(changed((mesh) => {
          part(mesh).color = colour;
        })), String(colour)).toThrow();
      }
      expect(() => parseMesh(changed((mesh) => {
        mesh.shapes.color = "#4c78a8";
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        part(mesh).subtype = "<b>solid</b>";
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        part(mesh).texture = { image: "https://example.com/x.png" };
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        mesh.shapes.title = IMG;
      }))).toThrow();
    });

    it("refuses a part map that names something else than the parts", () => {
      expect(() => parseMesh(changed((mesh) => {
        mesh.hotend.parts = { [IMG]: "/model/box" };
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        mesh.hotend.parts = { box: `/model/${IMG}` };
      }))).toThrow();
      expect(() => parseMesh(changed((mesh) => {
        mesh.hotend.parts = { box: "/model/lid" };
      }))).toThrow();
    });

    it("takes what the engine writes: parts, ghosts, a colour per face", () => {
      const mesh = parseMesh(changed((document) => {
        document.shapes.parts.push(
          { id: "/model/ghost.dumpling", name: "ghost.dumpling", type: "shapes", subtype: "solid", color: "#9aa7b4", alpha: 0.35, state: [1, 0], shape: { ref: 1 }, material: null, texture: null },
          { id: "/model/ghost.dumpling.2", name: "ghost.dumpling.2", color: ["#9aa7b4", "#9AA7B4"], shape: { ref: 1 } },
        );
      }));
      expect(mesh.shapes.parts).toHaveLength(5);
      // Ghosts are drawn and never picked: the part map names the parts only.
      expect(namedParts(mesh).map((one) => one.name)).toEqual(["box", "lid", "box_2"]);
    });

    it("gives the viewer no name from the layout: a piece is named by the window", () => {
      const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0];
      const scene = plateScene(parseMesh(MESH), [256, 256], [
        { name: IMG, part: "box", transform: identity },
        { name: "lid", part: `lid"><img src=x>`, transform: identity },
        { name: "lid.2", part: "lid", transform: identity },
      ]);
      // A piece of a part the mesh does not name is not drawn.
      expect(scene.shapes.parts.map((one) => [one.id, one.name])).toEqual([
        ["/model/hotend-plate", "hotend-plate"],
        ["/model/piece-1-box", "piece-1-box"],
        ["/model/piece-3-lid", "piece-3-lid"],
      ]);
      expect(JSON.stringify(scene.shapes)).not.toContain("<img");
      // Hotend still knows the piece by the layout's name; React writes that as text.
      expect((scene.hotend as { parts: Record<string, string> }).parts[IMG]).toBe("/model/piece-1-box");
      expect(() => {
        checkScene(coloured(scene, ["#111111"], () => "#abcdef"));
      }).not.toThrow();
    });

    it("checks once more what goes to the viewer, with the colours the window chose", () => {
      const scene = designScene(parseMesh(MESH));
      expect(() => {
        checkScene(coloured(scene, ["#111111", "#222222"], () => ""));
      }).not.toThrow();
      expect(() => {
        checkScene(coloured(scene, ['#111111"><img src=x>'], () => ""));
      }).toThrow();
      expect(() => {
        checkScene(coloured(plateScene(null, [256, 256], []), [], () => "rgb(1 2 3)"));
      }).toThrow();
      const hostile = { ...scene, shapes: { ...scene.shapes, parts: [{ ...scene.shapes.parts[0], id: `/model/${IMG}`, name: IMG }] } };
      expect(() => {
        checkScene(hostile);
      }).toThrow();
    });
  });

  it("reads a token colour as the number three.js takes", () => {
    expect(colourNumber(" #0B0C0E ")).toBe(0x0b0c0e);
    expect(colourNumber("#fff")).toBeNull();
    expect(colourNumber("red")).toBeNull();
  });
});
