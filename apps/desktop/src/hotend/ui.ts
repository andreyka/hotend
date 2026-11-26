// State of the studio that is the window's own and never the core's: which parts are
// hidden, which row is open, where the cut is, what the composer should take up. Kept
// apart from ./store.ts so that everything there stays "what the core said".
import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type { Picked } from "./viewport/Viewport";

export type CutAxis = "x" | "y" | "z";

export type Layout = { conversation: number; inspector: number; hideConversation: boolean; hideInspector: boolean };
export type Column = "conversation" | "inspector";

export const LAYOUT_DEFAULT: Layout = {
  conversation: 360,
  inspector: 340,
  hideConversation: false,
  hideInspector: false,
};
export const LAYOUT_LIMITS = { conversation: [300, 520], inspector: [300, 440] } as const;
/** The stage is never narrower than this (DESIGN_SPEC 3.2). */
export const STAGE_MIN = 520;
/** The line between two columns. */
const HANDLE = 1;

/**
 * The layout as it is drawn in a window `width` wide. `layout` is what the maker chose and
 * is kept as chosen; this is the one place where the three columns are made to fit, so
 * that their sum is never wider than the window. When there is too little room the
 * conversation gives way first, then the inspector, each down to its minimum. If both at
 * their minimum still do not fit, only one side column is shown: the one in `keep`, which
 * is the one the maker asked for last.
 */
export function fitLayout(layout: Layout, width: number, keep: Column = "inspector"): Layout {
  let { conversation, inspector, hideConversation, hideInspector } = layout;
  const room = () => width - STAGE_MIN - (hideConversation ? 0 : HANDLE) - (hideInspector ? 0 : HANDLE);
  const over = () => (hideConversation ? 0 : conversation) + (hideInspector ? 0 : inspector) - room();
  if (over() > 0 && !hideConversation) conversation = Math.max(LAYOUT_LIMITS.conversation[0], conversation - over());
  if (over() > 0 && !hideInspector) inspector = Math.max(LAYOUT_LIMITS.inspector[0], inspector - over());
  if (over() > 0 && !hideConversation && !hideInspector) {
    if (keep === "conversation") hideInspector = true;
    else hideConversation = true;
    // The column that stays gets back as much of its chosen width as there is room for.
    if (hideInspector) conversation = Math.max(LAYOUT_LIMITS.conversation[0], Math.min(layout.conversation, room()));
    else inspector = Math.max(LAYOUT_LIMITS.inspector[0], Math.min(layout.inspector, room()));
  }
  return { conversation, inspector, hideConversation, hideInspector };
}

const LAYOUT_KEY = "hotend.layout";

function storedLayout(): Layout {
  let stored: Partial<Layout> = {};
  try {
    stored = JSON.parse(window.localStorage.getItem(LAYOUT_KEY) ?? "{}") as Partial<Layout>;
  } catch {
    // The defaults then.
  }
  const clamp = (value: unknown, [min, max]: readonly [number, number], fallback: number) =>
    typeof value === "number" && Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : fallback;
  return {
    conversation: clamp(stored.conversation, LAYOUT_LIMITS.conversation, LAYOUT_DEFAULT.conversation),
    inspector: clamp(stored.inspector, LAYOUT_LIMITS.inspector, LAYOUT_DEFAULT.inspector),
    // A window narrower than 1220 px starts with the conversation hidden (3.2).
    hideConversation: stored.hideConversation ?? window.innerWidth < 1220,
    hideInspector: stored.hideInspector === true,
  };
}

export type Spot = { line: string; part: string | null; at: [number, number, number] | null; label: string | null };

export type Dimension = { name: string; from: [number, number, number]; to: [number, number, number]; label: string };

export type StudioUi = {
  /** The widths and hidden columns the maker chose. What is drawn is `shownLayout`. */
  layout: Layout;
  /** The width of the window, for fitting the columns into it. */
  width: number;
  /** The side column the maker asked for last: the one that stays when only one fits. */
  keep: Column;
  /** Parts hidden by their chips. */
  hidden: string[];
  /** The one part "Show me" leaves in view, or null. */
  solo: string | null;
  /**
   * Where "Show me" points: a place on the part with the measurement there, and the
   * Proof line it belongs to. What "Show me" hides comes back when that line closes,
   * when another version comes on the stage and when the maker leaves the stage.
   */
  spot: Spot | null;
  /** The row of "To measure" that is open; undefined until the maker or the window chose. */
  openSize: string | null | undefined;
  /** Counts up when a row's field should take the caret. */
  focusSize: number;
  /** Proof lines the maker opened or closed, by key. */
  openLines: Record<string, boolean>;
  /** What the inspector should scroll to, and a counter that makes each request new. */
  scroll: { to: "measure" | "problem" | "note"; tick: number } | null;
  /** Text for the composer to take up, and a counter that makes each request new. */
  draft: { text: string; tick: number } | null;
  cut: { on: boolean; axis: CutAxis; at: number };
  /** What the maker last clicked on the part. */
  picked: Picked | null;
  /** The size whose row is in use: its dimension is drawn on the part. */
  dimension: Dimension | null;
  /** The part whose row in Prepare is under the pointer or has the focus: outlined on the plate. */
  piece: string | null;
  /** Hotend wrote something while the conversation was hidden. */
  unseen: boolean;

  setLayout(change: Partial<Layout>): void;
  setWidth(width: number): void;
  /** Shows a side column that is hidden, by choice or for want of room. */
  showColumn(column: Column): void;
  /** The title-bar buttons and their keys: hides a column that is in view, shows one that is not. */
  toggleColumn(column: Column): void;
  togglePart(name: string): void;
  /** Every part back in view. */
  showAllParts(): void;
  showOnly(part: string | null, spot?: Spot | null): void;
  /** Ends "Show me": every part the chips do not hide is back, and the marker is gone. */
  endShow(): void;
  openSizeRow(name: string | null, focus?: boolean): void;
  toggleLine(key: string, open: boolean): void;
  scrollTo(to: "measure" | "problem" | "note"): void;
  fillComposer(text: string): void;
  setCut(change: Partial<StudioUi["cut"]>): void;
  setPicked(picked: Picked | null): void;
  setDimension(dimension: Dimension | null): void;
  setPiece(part: string | null): void;
  setUnseen(unseen: boolean): void;
  /** Forgets what belongs to one project when another opens. */
  reset(): void;
};

export const useStudioUi = create<StudioUi>()((set, get) => ({
  layout: storedLayout(),
  width: typeof window === "undefined" ? 1280 : window.innerWidth,
  keep: "inspector",
  hidden: [],
  solo: null,
  spot: null,
  openSize: undefined,
  focusSize: 0,
  openLines: {},
  scroll: null,
  draft: null,
  cut: { on: false, axis: "x", at: 0.5 },
  picked: null,
  dimension: null,
  piece: null,
  unseen: false,

  setLayout(change) {
    const layout = { ...get().layout, ...change };
    set({ layout });
    try {
      window.localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    } catch {
      // The layout then lasts until the window closes.
    }
  },
  setWidth(width) {
    if (width !== get().width) set({ width });
  },
  showColumn(column) {
    const hide = column === "conversation" ? "hideConversation" : "hideInspector";
    set({ keep: column });
    if (get().layout[hide]) get().setLayout({ [hide]: false });
  },
  toggleColumn(column) {
    const hide = column === "conversation" ? "hideConversation" : "hideInspector";
    if (shownLayout(get())[hide]) get().showColumn(column);
    else get().setLayout({ [hide]: true });
  },
  togglePart(name) {
    set((state) => ({
      solo: null,
      spot: null,
      hidden: state.hidden.includes(name) ? state.hidden.filter((part) => part !== name) : [...state.hidden, name],
    }));
  },
  showAllParts() {
    set({ hidden: [], solo: null, spot: null });
  },
  showOnly(part, spot = null) {
    set({ solo: part, spot: part === null ? null : spot });
  },
  endShow() {
    if (get().solo !== null || get().spot !== null) set({ solo: null, spot: null });
  },
  openSizeRow(name, focus = false) {
    set((state) => ({ openSize: name, focusSize: focus ? state.focusSize + 1 : state.focusSize }));
  },
  toggleLine(key, open) {
    set((state) => ({
      openLines: { ...state.openLines, [key]: open },
      // "Show me" lasts as long as its line is open.
      ...(!open && state.spot?.line === key ? { solo: null, spot: null } : {}),
    }));
  },
  scrollTo(to) {
    set((state) => ({ scroll: { to, tick: (state.scroll?.tick ?? 0) + 1 } }));
  },
  fillComposer(text) {
    set((state) => ({ draft: { text, tick: (state.draft?.tick ?? 0) + 1 } }));
  },
  setCut(change) {
    set((state) => ({ cut: { ...state.cut, ...change } }));
  },
  setPicked(picked) {
    set({ picked });
  },
  setPiece(part) {
    if (get().piece !== part) set({ piece: part });
  },
  setDimension(dimension) {
    const now = get().dimension;
    if (dimension === null ? now !== null : now?.name !== dimension.name || now.label !== dimension.label) set({ dimension });
  },
  setUnseen(unseen) {
    set({ unseen });
  },
  reset() {
    set({ hidden: [], solo: null, spot: null, openSize: undefined, openLines: {}, scroll: null, draft: null, picked: null, dimension: null, piece: null, unseen: false, cut: { on: false, axis: "x", at: 0.5 } });
  },
}));

/** The layout as drawn: the maker's, fitted into the window. */
export function shownLayout(state: Pick<StudioUi, "layout" | "width" | "keep">): Layout {
  return fitLayout(state.layout, state.width, state.keep);
}

/** `shownLayout` for a component; the object changes only when one of its fields does. */
export function useShownLayout(): Layout {
  return useStudioUi(useShallow(shownLayout));
}
