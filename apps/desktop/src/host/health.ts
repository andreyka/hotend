// The doctor report as the four rows of "This computer" (DESIGN_SPEC 4.1): each row is
// the worst status of a group of the report's checks.
import type { DoctorCheck, DoctorReport, DoctorStatus } from "@/lib/core";

import { H } from "./strings";

export type HealthStatus = "ok" | "warn" | "fail";
export type HealthRow = {
  id: string;
  label: string;
  status: HealthStatus;
  /** What the row says, and under it what to do. */
  text: string;
  fix: string | null;
  /** Where in the window the row is put right, when that is a screen of Hotend's. */
  link: { to: string; label: string } | null;
  required: boolean;
};

const GROUPS: { id: keyof typeof H.health.rows; has(id: string): boolean }[] = [
  { id: "sandbox", has: (id) => id === "sandbox" || id.startsWith("sandbox.") },
  { id: "engine", has: (id) => id === "python" || id === "engine" || id === "agent" },
  { id: "slicer", has: (id) => id.startsWith("slicer.") },
  { id: "secrets", has: (id) => id.startsWith("secrets.") },
];

function rank(status: DoctorStatus): number {
  return status === "fail" ? 2 : status === "warn" ? 1 : 0;
}

function worst(checks: readonly DoctorCheck[]): DoctorCheck | null {
  return checks.reduce<DoctorCheck | null>((found, check) => (found === null || rank(check.status) > rank(found.status) ? check : found), null);
}

function statusOf(check: DoctorCheck | null): HealthStatus {
  return check === null ? "ok" : check.status === "fail" ? "fail" : check.status === "warn" ? "warn" : "ok";
}

export function healthRows(report: DoctorReport): HealthRow[] {
  const rows: HealthRow[] = [];
  const grouped = new Set<string>();
  for (const group of GROUPS) {
    const checks = report.checks.filter((check) => group.has(check.id));
    checks.forEach((check) => grouped.add(check.id));
    if (checks.length === 0) continue;
    const bad = worst(checks);
    const status = statusOf(bad);
    const required = checks.some((check) => check.required);
    let text: string;
    let fix: string | null = status === "ok" ? null : (bad?.fix ?? null);
    let link: HealthRow["link"] = null;
    if (group.id === "sandbox") {
      text = status === "ok" ? H.health.sandboxOk : (bad?.detail ?? "");
    } else if (group.id === "engine") {
      const seconds = /in ([\d.]+) s/.exec(checks.find((check) => check.id === "engine")?.detail ?? "")?.[1] ?? null;
      text = status === "ok" ? H.health.engineOk(seconds) : (bad?.detail ?? "");
    } else if (group.id === "slicer") {
      const real = report.slicers.find((slicer) => slicer.usable && !slicer.stand_in);
      const standIn = report.slicers.some((slicer) => slicer.usable && slicer.stand_in);
      text = real ? H.health.slicerOk([real.name, real.version].filter(Boolean).join(" ")) : standIn ? H.health.slicerStandIn : H.health.slicerNone;
      fix = null;
      // No slicer at all: nothing can be sliced, and Settings, Slicer is where one is chosen.
      if (!real && !standIn) link = { to: "/settings/slicer", label: H.health.slicerOpen };
    } else {
      const keyring = checks.find((check) => check.id === "secrets.keyring");
      const file = checks.find((check) => check.id === "secrets.file");
      const fromEnv = /Default store now: environment/i.test(checks.find((check) => check.id === "secrets.env")?.detail ?? "");
      if (file) {
        // The person chose the file store: its own check says whether it can be used.
        text = file.status === "ok" ? H.health.secretsFile : file.detail;
        fix = file.status === "ok" ? null : file.fix;
      } else {
        text = !keyring || keyring.status === "ok" ? H.health.secretsOk : fromEnv ? H.health.secretsEnv : H.health.secretsNone;
        fix = null;
      }
    }
    rows.push({ id: group.id, label: H.health.rows[group.id], status, text, fix, link, required });
  }
  // A check with an unknown id gets a row of its own, with the report's label.
  for (const check of report.checks) {
    if (grouped.has(check.id)) continue;
    rows.push({
      id: check.id,
      label: check.label.trim().replace(/^./, (first) => first.toUpperCase()),
      status: statusOf(check),
      text: check.detail.replace(/^./, (first) => first.toUpperCase()),
      fix: check.fix,
      link: null,
      required: check.required,
    });
  }
  return rows;
}

/** The first required check that failed: Hotend then runs no AI-written model at all. */
export function failedRequired(report: DoctorReport | null): DoctorCheck | null {
  return report?.checks.find((check) => check.required && check.status === "fail") ?? null;
}

/** True when Hotend keeps keys in its secrets file (started with HOTEND_SECRETS=file). */
export function keysInFile(report: DoctorReport | null): boolean {
  return report?.checks.some((check) => check.id === "secrets.file") ?? false;
}

/**
 * True when no key can be stored on this computer: the keychain cannot be used and Hotend
 * was not started with the file store instead.
 */
export function noKeychain(report: DoctorReport | null): boolean {
  if (keysInFile(report)) return false;
  const keyring = report?.checks.find((check) => check.id === "secrets.keyring");
  return keyring !== undefined && keyring.status !== "ok";
}
