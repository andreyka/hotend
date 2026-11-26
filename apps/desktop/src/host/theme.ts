// The theme is the `data-theme` attribute on the root element (DESIGN_SPEC 7.1). "System"
// is resolved here to one of the two; the stylesheet never reads the system setting.
import { create } from "zustand";

export type ThemeChoice = "dark" | "light" | "system";
export type Theme = "dark" | "light";

const STORAGE_KEY = "app.theme";

// Every browser has matchMedia; a test environment may not.
const systemLight =
  typeof window.matchMedia === "function" ? window.matchMedia("(prefers-color-scheme: light)") : null;

function systemTheme(): Theme {
  return systemLight?.matches ? "light" : "dark";
}

export function resolveTheme(choice: ThemeChoice, system: Theme): Theme {
  return choice === "system" ? system : choice;
}

function storedChoice(): ThemeChoice {
  try {
    const stored = window.localStorage.getItem(STORAGE_KEY);
    return stored === "light" || stored === "system" ? stored : "dark";
  } catch {
    return "dark";
  }
}

type ThemeState = {
  choice: ThemeChoice;
  theme: Theme;
  choose(choice: ThemeChoice): void;
};

function apply(theme: Theme) {
  const root = document.documentElement;
  if (root.dataset.theme === theme) return;
  // Nothing fades from one theme to the other: for two frames no transition runs
  // (styles/app.css), so every control is in the new theme's colours at once.
  root.dataset.themeSwitching = "";
  root.dataset.theme = theme;
  const frame = typeof requestAnimationFrame === "function" ? requestAnimationFrame : (run: () => void) => setTimeout(run, 16);
  frame(() => {
    frame(() => {
      delete root.dataset.themeSwitching;
    });
  });
}

export const useTheme = create<ThemeState>()((set) => {
  const choice = storedChoice();
  const theme = resolveTheme(choice, systemTheme());
  apply(theme);
  systemLight?.addEventListener("change", () => {
    set((state) => {
      const next = resolveTheme(state.choice, systemTheme());
      apply(next);
      return { theme: next };
    });
  });
  return {
    choice,
    theme,
    choose(next) {
      try {
        window.localStorage.setItem(STORAGE_KEY, next);
      } catch {
        // The choice then lasts until the window closes.
      }
      const resolved = resolveTheme(next, systemTheme());
      apply(resolved);
      set({ choice: next, theme: resolved });
    },
  };
});
