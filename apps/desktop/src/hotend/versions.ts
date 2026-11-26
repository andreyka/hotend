// What changed between two versions (DESIGN_SPEC 4.3.1 "Version markers", 5.5). Sizes
// come from the versions' params; parts and proof need both engine results.
import type { EngineResult, PartSummary, Version } from "@/lib/core";

import { dimensions, sized, weight } from "./format";
import { countsText, proofOf, standing, statusLine, type Proof } from "./proof";
import { S } from "./strings";

export type SizeChange = { name: string; label: string; unit: string; from: number; to: number };
export type PartChange = { name: string; size: string | null; mass: string | null };

export type VersionDiff = {
  sizes: SizeChange[];
  parts: PartChange[];
  nowPasses: string[];
  nowFails: string[];
  /** "Same proof: 10 pass · 3 notes." when no proof line changed state. */
  sameProof: string | null;
};

function label(name: string, ...results: (EngineResult | null | undefined)[]): { label: string; unit: string } {
  for (const result of results) {
    const spec = result?.param_specs?.[name];
    if (spec) return { label: spec.label || name, unit: spec.unit };
  }
  return { label: name, unit: "mm" };
}

export function sizeChanges(
  before: Version | null | undefined,
  after: Version,
  afterResult?: EngineResult | null,
  beforeResult?: EngineResult | null,
): SizeChange[] {
  if (!before) return [];
  return Object.entries(after.params).flatMap(([name, to]) => {
    const from = before.params[name];
    if (from === undefined || from === to) return [];
    return [{ name, ...label(name, afterResult, beforeResult), from, to }];
  });
}

function extent(part: PartSummary): number[] {
  return part.bbox.max.map((max, axis) => max - (part.bbox.min[axis] ?? 0));
}

function partChanges(before: readonly PartSummary[], after: readonly PartSummary[]): PartChange[] {
  return after.flatMap((part) => {
    const old = before.find((candidate) => candidate.name === part.name);
    if (!old) return [];
    const was = dimensions(extent(old));
    const now = dimensions(extent(part));
    const size = was === now ? null : `${was.replace(/ mm$/, "")} → ${now}`;
    const massWas = old.mass_estimate_g === null ? null : weight(old.mass_estimate_g);
    const massNow = part.mass_estimate_g === null ? null : weight(part.mass_estimate_g);
    const mass =
      massWas === null || massNow === null || massWas === massNow
        ? null
        : `${massWas.replace(/ g$/, "")} → ${massNow}`;
    return size === null && mass === null ? [] : [{ name: part.name, size, mass }];
  });
}

function proofChanges(before: Proof, after: Proof): { nowPasses: string[]; nowFails: string[] } {
  const was = new Map(before.groups.flatMap((group) => group.lines).map((line) => [line.key, standing(line)]));
  const lines = after.groups.flatMap((group) => group.lines);
  const changed = lines.filter((line) => was.has(line.key) && was.get(line.key) !== standing(line));
  return {
    nowPasses: changed.filter((line) => standing(line) === "pass").map((line) => line.title),
    nowFails: changed.filter((line) => standing(line) === "problem").map((line) => line.title),
  };
}

/** What a version changed against the one before it. */
export function diffVersions(
  before: { version: Version; result: EngineResult | null | undefined } | null,
  after: { version: Version; result: EngineResult | null | undefined },
): VersionDiff {
  const afterProof = proofOf(after.result);
  if (!before) {
    return { sizes: [], parts: [], nowPasses: [], nowFails: [], sameProof: null };
  }
  const changes = proofChanges(proofOf(before.result), afterProof);
  const same = changes.nowPasses.length === 0 && changes.nowFails.length === 0;
  return {
    sizes: sizeChanges(before.version, after.version, after.result, before.result),
    parts: partChanges(before.result?.parts ?? [], after.result?.parts ?? []),
    ...changes,
    sameProof: same && after.result ? S.rail.sameProof(countsText(afterProof.counts)) : null,
  };
}

/** "Hotend" for a written model, "Height set to 46 mm" for one changed size. */
export function versionOrigin(
  version: Version,
  parent: Version | null | undefined,
  result?: EngineResult | null,
): string {
  if (version.origin !== "set_parameter") return S.marker.written;
  const change = sizeChanges(parent, version, result)[0];
  if (change) return S.marker.sizeSet(change.label, sized(change.to, change.unit));
  // The same sizes, built again: what "Rebuild with measurements" does after a calibration.
  const names = Object.keys(version.params);
  const same = parent != null && names.length === Object.keys(parent.params).length && names.every((name) => parent.params[name] === version.params[name]);
  return same ? S.marker.builtAgain : S.marker.sizeChanged;
}

/** "v2 · Hotend", "v3 · Height set to 46 mm". */
export function versionLabel(
  version: Version,
  parent: Version | null | undefined,
  result?: EngineResult | null,
): string {
  return `v${version.number} · ${versionOrigin(version, parent, result)}`;
}

/** The lines under a marker in the conversation: at most three. */
export function markerLines(
  before: { version: Version; result: EngineResult | null | undefined } | null,
  after: { version: Version; result: EngineResult | null | undefined },
): string[] {
  if (!after.result) return [];
  if (!before) return [statusLine(proofOf(after.result)).text];
  const diff = diffVersions(before, after);
  const lines: string[] = [];
  if (after.version.origin !== "set_parameter") {
    for (const size of diff.sizes) {
      lines.push(S.marker.sizeChange(size.label, sized(size.from, size.unit).replace(/ mm$/, ""), sized(size.to, size.unit)));
    }
  }
  if (diff.nowFails.length > 0) lines.push(S.marker.nowFails(diff.nowFails.join(", ")));
  if (diff.nowPasses.length > 0) lines.push(S.marker.nowPasses(diff.nowPasses.join(", ")));
  if (lines.length === 0) return [diff.sameProof ?? S.marker.same];
  return lines.slice(0, 3);
}

/** The accessible name of a dot on the rail. */
export function dotName(version: Version, result: EngineResult | null | undefined): string {
  const how = version.origin === "set_parameter" ? S.rail.dotSize : S.rail.dotWritten;
  const proof = result ? proofOf(result) : null;
  const state = proof && proof.problems > 0 ? `, ${S.rail.dotProblems(proof.problems)}` : "";
  return S.rail.dotName(version.number, how) + state;
}
