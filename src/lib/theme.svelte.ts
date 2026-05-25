import { api } from "./tauri";

export type Theme = "light" | "dark" | "system";

let _storedTheme = $state<Theme>("system");
let _effectiveTheme = $state<"light" | "dark">("light");

export function getStoredTheme(): Theme {
  return _storedTheme;
}

export function getEffectiveTheme(): "light" | "dark" {
  return _effectiveTheme;
}
let _initialized = false;

function resolveTheme(stored: Theme): "light" | "dark" {
  if (stored === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return stored;
}

function applyTheme(effective: "light" | "dark") {
  document.documentElement.setAttribute("data-theme", effective);
}

export async function init(): Promise<void> {
  if (_initialized) return;
  _initialized = true;

  try {
    const theme = await api.getSetting("theme");
    if (theme === "light" || theme === "dark" || theme === "system") {
      _storedTheme = theme;
    }
  } catch {}

  _effectiveTheme = resolveTheme(_storedTheme);
  applyTheme(_effectiveTheme);

  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
    if (_storedTheme === "system") {
      _effectiveTheme = e.matches ? "dark" : "light";
      applyTheme(_effectiveTheme);
    }
  });
}

export async function setTheme(theme: Theme): Promise<void> {
  _storedTheme = theme;
  _effectiveTheme = resolveTheme(theme);
  applyTheme(_effectiveTheme);
  try {
    await api.setSetting("theme", theme);
  } catch {}
}
