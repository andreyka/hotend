// The printer's AMS units and trays (DESIGN_SPEC 4.7), in the printer chip and beside a
// running print. The core counts units and trays from 0; people count from 1.
import { Badge } from "@/components/ui/badge";
import { useTheme } from "@/host/theme";
import type { AmsInfo } from "@/lib/core";

import { S } from "./strings";

/** "#FFFFFFFF" of the printer as a CSS colour, or null when it states none. */
export function swatchColour(colour: string | null | undefined): string | null {
  if (!colour) return null;
  const hex = colour.replace(/^#/, "");
  return /^[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(hex) ? `#${hex}` : null;
}

/** Relative luminance of "#RRGGBB" (WCAG). */
function luminance(css: string): number {
  const channel = (at: number) => {
    const value = Number.parseInt(css.slice(at, at + 2), 16) / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
}

/** True when two colours are within 1.3 : 1 of each other: one would vanish on the other. */
export function nearColour(a: string, b: string): boolean {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return ((light ?? 0) + 0.05) / ((dark ?? 0) + 0.05) < 1.3;
}

/**
 * The colour of a spool: a small square with its own hairline, never a circle, which
 * next to a label reads as a radio button. A colour close to the surface it lies on gets
 * an outer ring too.
 */
export function Swatch({ colour }: { colour: string | null | undefined }) {
  const css = swatchColour(colour);
  const theme = useTheme((state) => state.theme);
  const name = css ? S.print.spoolColour(css.slice(0, 7).toUpperCase()) : S.print.spoolNoColour;
  // The surfaces a swatch lies on: the raised near-white or near-black of a card or a popover.
  const near = css !== null && nearColour(css.slice(0, 7), theme === "light" ? "#F7F8F9" : "#1B2126");
  return (
    <span
      role="img"
      aria-label={name}
      title={name}
      data-testid="swatch"
      data-near={near || css === null || undefined}
      className="app-swatch"
      style={css ? { background: css } : undefined}
    />
  );
}

export function AmsList({
  units,
  inUse,
}: {
  units: readonly AmsInfo[];
  /** The trays the running job draws from: AMS unit and tray, counted from 0. */
  inUse?: readonly { unit: number; tray: number }[];
}) {
  if (units.length === 0) return null;
  // Units are numbered in the order the printer lists them; the HT unit has its own name.
  const ordinals = units.map((_, index) => units.slice(0, index + 1).filter((other) => !other.high_temperature).length);
  return (
    <div className="space-y-2" data-testid="ams">
      {units.map((unit, index) => {
        const ordinary = ordinals[index] ?? 0;
        return (
          <div key={unit.id}>
            <p className="t-caption py-1 text-app-text-2">{S.print.ams(ordinary, unit.high_temperature, unit.nozzle_id)}</p>
            <ul>
              {unit.trays.map((tray) => (
                <li key={tray.id} className="t-ui num flex min-h-6 items-center gap-2">
                  <span className="w-3 text-app-text-2">{tray.id + 1}</span>
                  <Swatch colour={tray.colour} />
                  <span className="min-w-0 flex-1 truncate">{tray.product ?? tray.material ?? S.print.empty}</span>
                  {inUse?.some((spool) => spool.unit === unit.id && spool.tray === tray.id) ? (
                    <span className="shrink-0" data-testid="spool-in-use">
                      <Badge tone="accent">{S.print.inUse}</Badge>
                    </span>
                  ) : null}
                  {tray.remaining_percent === null ? null : (
                    <span className="shrink-0 text-app-text-2">{S.print.spoolLeft(Math.round(tray.remaining_percent))}</span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        );
      })}
    </div>
  );
}
