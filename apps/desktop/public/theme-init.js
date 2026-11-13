/*
 * Pre-paint theme bootstrap. Loaded synchronously from index.html <head> so
 * the .dark class (and color-scheme) are on <html> before the first paint —
 * no white flash when the app opens in dark mode. Keep in sync with
 * src/lib/theme.ts (storage key + resolution rules). Served as a same-origin
 * file so the packaged app's CSP (`script-src 'self'`) permits it.
 */
(function () {
  "use strict";
  var KEY = "any-converter.desktop.theme";
  var stored;
  try {
    stored = localStorage.getItem(KEY);
  } catch (error) {
    stored = null;
  }
  var prefersDark =
    stored === "dark" ||
    ((stored === null || stored === "system") &&
      window.matchMedia("(prefers-color-scheme: dark)").matches);
  var root = document.documentElement;
  root.classList.toggle("dark", prefersDark);
  root.style.colorScheme = prefersDark ? "dark" : "light";
})();
