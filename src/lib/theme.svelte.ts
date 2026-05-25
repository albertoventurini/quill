import { api } from "./tauri";

export type Theme = "light" | "dark" | "system";

export let storedTheme = $state<Theme>("system");
export let effectiveTheme = $state<"light" | "dark">("light");
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
      storedTheme = theme;
    }
  } catch {}

  effectiveTheme = resolveTheme(storedTheme);
  applyTheme(effectiveTheme);

  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
    if (storedTheme === "system") {
      effectiveTheme = e.matches ? "dark" : "light";
      applyTheme(effectiveTheme);
    }
  });
}

export async function setTheme(theme: Theme): Promise<void> {
  storedTheme = theme;
  effectiveTheme = resolveTheme(theme);
  applyTheme(effectiveTheme);
  try {
    await api.setSetting("theme", theme);
  } catch {}
}
