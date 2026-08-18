/**
 * Synchronous FOUC guard for popup/options pages.
 * Reads the same localStorage key written by applyExtensionAppearance().
 * Cache is last-known desktop appearance (no network / native messaging here).
 */
(function () {
  const CACHE_KEY = 'rusticdl-appearance';
  const DEFAULT_ACCENT_COLOR = '#3b82f6';
  const DEFAULT_APPEARANCE = { theme: 'light', accentColor: DEFAULT_ACCENT_COLOR };
  const root = document.documentElement;

  function normalizeAccentColor(rawColor) {
    const color = typeof rawColor === 'string' ? rawColor.trim() : '';
    return /^#[0-9a-f]{6}$/i.test(color) ? color.toLowerCase() : DEFAULT_ACCENT_COLOR;
  }

  function normalizeTheme(theme) {
    return theme === 'light' || theme === 'dark' || theme === 'system'
      ? theme
      : DEFAULT_APPEARANCE.theme;
  }

  function normalizeAppearance(settings) {
    return {
      theme: normalizeTheme(settings && settings.theme),
      accentColor: normalizeAccentColor(settings && settings.accentColor),
    };
  }

  function cachedAppearance() {
    try {
      return normalizeAppearance(JSON.parse(localStorage.getItem(CACHE_KEY) || 'null'));
    } catch {
      return DEFAULT_APPEARANCE;
    }
  }

  function systemPrefersDark() {
    return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches;
  }

  function readableForegroundForHex(hex) {
    const normalized = normalizeAccentColor(hex);
    const red = Number.parseInt(normalized.slice(1, 3), 16);
    const green = Number.parseInt(normalized.slice(3, 5), 16);
    const blue = Number.parseInt(normalized.slice(5, 7), 16);
    const luminance = (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255;
    return luminance > 0.58 ? '#0a0f14' : '#ffffff';
  }

  const settings = cachedAppearance();
  const dark =
    settings.theme === 'dark'
    || (settings.theme === 'system' && systemPrefersDark());
  const accent = normalizeAccentColor(settings.accentColor);

  root.classList.toggle('light', settings.theme === 'light');
  root.classList.toggle('dark', dark);
  root.classList.remove('oled-dark');
  root.style.setProperty('--color-primary', accent);
  root.style.setProperty('--color-ring', accent);
  root.style.setProperty('--color-primary-foreground', readableForegroundForHex(accent));
  root.style.setProperty(
    '--color-primary-soft',
    `color-mix(in oklch, ${accent} 20%, var(--color-background))`,
  );
  root.style.setProperty(
    '--color-accent',
    `color-mix(in oklch, ${accent} 20%, var(--color-background))`,
  );
  root.style.setProperty('--color-accent-foreground', accent);
  root.style.setProperty(
    '--color-selected',
    `color-mix(in oklch, ${accent} 24%, var(--color-background))`,
  );
})();
