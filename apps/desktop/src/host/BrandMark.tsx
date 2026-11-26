import { useId } from "react";

import { H } from "./strings";

// Hotend's mark: the hot end of a printer and the bead it lays. The same shapes as
// public/favicon.svg, in the text colour of the theme; the bead's heat is the same in both.
// Not smaller than 16 px, with a clear space of at least the height of the hex band
// (5.5 of its 48 units) around it.
export function BrandMark({
  size = 20,
  className,
  decorative = false,
}: {
  size?: number;
  className?: string;
  /** Beside running text that already says the name. */
  decorative?: boolean;
}) {
  // Two marks can be on one screen: each has a gradient of its own.
  const heat = `heat-${useId().replace(/\W/g, "")}`;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 48 48"
      className={className}
      data-testid="brand-mark"
      {...(decorative ? { "aria-hidden": true } : { role: "img", "aria-label": H.app })}
    >
      <defs>
        <linearGradient id={heat} x1={4} y1={0} x2={27} y2={0} gradientUnits="userSpaceOnUse">
          <stop offset={0} className="[stop-color:var(--app-text)]" />
          <stop offset={0.42} className="[stop-color:var(--app-heat-3)]" />
          <stop offset={0.78} className="[stop-color:var(--app-heat-2)]" />
          <stop offset={1} className="[stop-color:var(--app-heat-1)]" />
        </linearGradient>
      </defs>
      <g className="fill-app-text">
        <rect x={9} y={4} width={30} height={13} rx={3.5} />
        <rect x={14} y={19.5} width={20} height={5.5} rx={1} />
        <path d="M15.5 27.5h17L26.4 37h-4.8z" />
      </g>
      <rect x={4} y={40} width={23} height={4} rx={2} fill={`url(#${heat})`} />
    </svg>
  );
}

/** The word of the mark, beside it. It is lowercase to look at and read out once, by the mark. */
export function Wordmark() {
  return (
    <span className="app-wordmark" aria-hidden data-testid="wordmark">
      hotend
    </span>
  );
}
