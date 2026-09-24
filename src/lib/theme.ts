export type ThemeMode = "system" | "light" | "dark";

const KEY = "synapse-theme";

export function getTheme(): ThemeMode {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

function apply(mode: ThemeMode) {
  const root = document.documentElement;
  if (mode === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", mode);
}

export function setTheme(mode: ThemeMode) {
  try {
    if (mode === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, mode);
  } catch {
    /* ignore */
  }
  apply(mode);
}

export function initTheme() {
  apply(getTheme());
  window.addEventListener("storage", (e) => {
    if (e.key === KEY) apply(getTheme());
  });
}
