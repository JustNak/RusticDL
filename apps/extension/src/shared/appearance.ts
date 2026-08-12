import type { AppearanceSettings, AppearanceTheme } from '@rusticdl/protocol';

export const DEFAULT_ACCENT_COLOR = '#3b82f6';
/** localStorage key for FOUC-free paint in popup/options pages. */
export const APPEARANCE_CACHE_KEY = 'rusticdl-appearance';
/** browser.storage.local key — cached desktop appearance for the extension. */
export const APPEARANCE_STORAGE_KEY = 'appearance-settings';

export const DEFAULT_APPEARANCE_SETTINGS: AppearanceSettings = {
  theme: 'system',
  accentColor: DEFAULT_ACCENT_COLOR,
};

export function normalizeAccentColor(rawColor: string | undefined): string {
  const color = rawColor?.trim() ?? '';
  return /^#[0-9a-f]{6}$/i.test(color) ? color.toLowerCase() : DEFAULT_ACCENT_COLOR;
}

export function normalizeAppearanceSettings(
  settings?: Partial<AppearanceSettings> | null,
): AppearanceSettings {
  return {
    theme: normalizeTheme(settings?.theme),
    accentColor: normalizeAccentColor(settings?.accentColor),
  };
}

export function readableForegroundForHex(hex: string): string {
  const normalized = normalizeAccentColor(hex);
  const red = Number.parseInt(normalized.slice(1, 3), 16);
  const green = Number.parseInt(normalized.slice(3, 5), 16);
  const blue = Number.parseInt(normalized.slice(5, 7), 16);
  const luminance = (0.2126 * red + 0.7152 * green + 0.0722 * blue) / 255;
  return luminance > 0.58 ? '#0a0f14' : '#ffffff';
}

function normalizeTheme(theme: AppearanceTheme | undefined): AppearanceTheme {
  return theme === 'light' || theme === 'dark' || theme === 'system'
    ? theme
    : DEFAULT_APPEARANCE_SETTINGS.theme;
}

/** Persist a FOUC cache so the next open paints with the last theme immediately. */
export function cacheAppearanceLocally(settings: AppearanceSettings): void {
  try {
    window.localStorage?.setItem(APPEARANCE_CACHE_KEY, JSON.stringify(settings));
  } catch {
    // ignore quota / private mode
  }
}

export function readCachedAppearance(): AppearanceSettings {
  try {
    const raw = window.localStorage?.getItem(APPEARANCE_CACHE_KEY);
    if (!raw) return { ...DEFAULT_APPEARANCE_SETTINGS };
    return normalizeAppearanceSettings(JSON.parse(raw) as Partial<AppearanceSettings>);
  } catch {
    return { ...DEFAULT_APPEARANCE_SETTINGS };
  }
}

export function applyExtensionAppearance(settings: Partial<AppearanceSettings> | undefined): void {
  const root = document.documentElement;
  const normalized = normalizeAppearanceSettings(settings);
  const systemPrefersDark =
    typeof window !== 'undefined'
    && typeof window.matchMedia === 'function'
    && window.matchMedia('(prefers-color-scheme: dark)').matches;
  const dark = normalized.theme === 'dark' || (normalized.theme === 'system' && systemPrefersDark);
  const accent = normalizeAccentColor(normalized.accentColor);
  const foreground = readableForegroundForHex(accent);

  root.classList.toggle('light', normalized.theme === 'light');
  root.classList.toggle('dark', dark);
  root.classList.remove('oled-dark');
  root.style.setProperty('--color-primary', accent);
  root.style.setProperty('--color-ring', accent);
  root.style.setProperty('--color-primary-foreground', foreground);
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

  cacheAppearanceLocally(normalized);
}
