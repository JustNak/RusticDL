import type { AppearanceSettings, AppearanceTheme, ExtensionIntegrationSettings } from '@rusticdl/protocol';
import browser from 'webextension-polyfill';
import type { PopupRequest, PopupStateResponse } from '../shared/messages';
import { createDefaultExtensionSettings } from '../shared/defaultExtensionSettings';
import {
  ACCENT_PRESETS,
  applyExtensionAppearance,
  DEFAULT_APPEARANCE_SETTINGS,
  normalizeAccentColor,
  normalizeAppearanceSettings,
  readCachedAppearance,
} from '../shared/appearance';

const enabled = document.querySelector<HTMLInputElement>('#enabled');
const contextMenu = document.querySelector<HTMLInputElement>('#context-menu');
const badge = document.querySelector<HTMLInputElement>('#badge');
const silent = document.querySelector<HTMLInputElement>('#silent');
const excludedHosts = document.querySelector<HTMLTextAreaElement>('#excluded-hosts');
const capturedExtensions = document.querySelector<HTMLInputElement>('#captured-extensions');
const saveButton = document.querySelector<HTMLButtonElement>('#save');
const resetAppearanceButton = document.querySelector<HTMLButtonElement>('#reset-appearance');
const status = document.querySelector<HTMLElement>('#status');
const accentColorInput = document.querySelector<HTMLInputElement>('#accent-color');
const accentHexInput = document.querySelector<HTMLInputElement>('#accent-hex');
const swatchHost = document.querySelector<HTMLElement>('#accent-swatches');

let draftAppearance: AppearanceSettings = normalizeAppearanceSettings(readCachedAppearance());
let appearanceDirty = false;

// Immediate paint from local cache (no desktop dependency).
applyExtensionAppearance(draftAppearance);

async function sendMessage<T>(message: PopupRequest): Promise<T> {
  return browser.runtime.sendMessage(message) as Promise<T>;
}

function fillCaptureForm(settings: ExtensionIntegrationSettings) {
  if (enabled) enabled.checked = settings.enabled;
  if (contextMenu) contextMenu.checked = settings.contextMenuEnabled;
  if (badge) badge.checked = settings.showBadgeStatus;
  if (silent) silent.checked = settings.downloadHandoffMode === 'auto';
  if (excludedHosts) excludedHosts.value = settings.excludedHosts.join('\n');
  if (capturedExtensions) capturedExtensions.value = settings.capturedFileExtensions.join(', ');
}

function readCaptureForm(base: ExtensionIntegrationSettings): ExtensionIntegrationSettings {
  return {
    ...base,
    enabled: enabled?.checked ?? base.enabled,
    contextMenuEnabled: contextMenu?.checked ?? base.contextMenuEnabled,
    showBadgeStatus: badge?.checked ?? base.showBadgeStatus,
    downloadHandoffMode: silent?.checked ? 'auto' : 'ask',
    excludedHosts: (excludedHosts?.value ?? '')
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean),
    capturedFileExtensions: (capturedExtensions?.value ?? '')
      .split(/[,\s]+/)
      .map((value) => value.trim())
      .filter(Boolean),
  };
}

function selectedTheme(): AppearanceTheme {
  const checked = document.querySelector<HTMLInputElement>('input[name="theme"]:checked');
  const value = checked?.value;
  return value === 'light' || value === 'dark' || value === 'system' ? value : 'system';
}

function fillAppearanceForm(settings: AppearanceSettings) {
  draftAppearance = normalizeAppearanceSettings(settings);
  for (const radio of Array.from(document.querySelectorAll<HTMLInputElement>('input[name="theme"]'))) {
    radio.checked = radio.value === draftAppearance.theme;
  }
  if (accentColorInput) accentColorInput.value = draftAppearance.accentColor;
  if (accentHexInput) accentHexInput.value = draftAppearance.accentColor;
  updateSwatchSelection(draftAppearance.accentColor);
  applyExtensionAppearance(draftAppearance);
}

function updateSwatchSelection(color: string) {
  if (!swatchHost) return;
  const normalized = normalizeAccentColor(color);
  for (const button of Array.from(swatchHost.querySelectorAll<HTMLButtonElement>('.swatch'))) {
    button.setAttribute('aria-pressed', button.dataset.color === normalized ? 'true' : 'false');
  }
}

function setDraftAppearance(patch: Partial<AppearanceSettings>, markDirty = true) {
  draftAppearance = normalizeAppearanceSettings({ ...draftAppearance, ...patch });
  if (accentColorInput) accentColorInput.value = draftAppearance.accentColor;
  if (accentHexInput) accentHexInput.value = draftAppearance.accentColor;
  updateSwatchSelection(draftAppearance.accentColor);
  applyExtensionAppearance(draftAppearance);
  if (markDirty) appearanceDirty = true;
}

function buildSwatches() {
  if (!swatchHost) return;
  swatchHost.replaceChildren();
  for (const preset of ACCENT_PRESETS) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'swatch';
    button.title = preset.label;
    button.setAttribute('aria-label', preset.label);
    button.dataset.color = preset.color;
    button.style.background = preset.color;
    button.addEventListener('click', () => {
      setDraftAppearance({ accentColor: preset.color });
    });
    swatchHost.append(button);
  }
}

async function saveAppearance(): Promise<AppearanceSettings> {
  const next = normalizeAppearanceSettings({
    theme: selectedTheme(),
    accentColor: accentHexInput?.value ?? draftAppearance.accentColor,
  });
  const state = await sendMessage<PopupStateResponse>({
    type: 'appearance_settings_update',
    settings: next,
  });
  const saved = state.appearanceSettings ?? next;
  fillAppearanceForm(saved);
  appearanceDirty = false;
  return saved;
}

for (const radio of Array.from(document.querySelectorAll<HTMLInputElement>('input[name="theme"]'))) {
  radio.addEventListener('change', () => {
    setDraftAppearance({ theme: selectedTheme() });
  });
}

accentColorInput?.addEventListener('input', () => {
  setDraftAppearance({ accentColor: accentColorInput.value });
});

accentHexInput?.addEventListener('change', () => {
  setDraftAppearance({ accentColor: accentHexInput.value });
});

accentHexInput?.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') {
    event.preventDefault();
    setDraftAppearance({ accentColor: accentHexInput.value });
  }
});

resetAppearanceButton?.addEventListener('click', async () => {
  if (status) status.textContent = 'Resetting appearance…';
  fillAppearanceForm(DEFAULT_APPEARANCE_SETTINGS);
  await saveAppearance();
  if (status) status.textContent = 'Appearance reset to defaults (local to this extension).';
});

saveButton?.addEventListener('click', async () => {
  if (status) status.textContent = 'Saving…';

  const state = await sendMessage<PopupStateResponse>({ type: 'popup_get_state' });
  const base = state.extensionSettings ?? createDefaultExtensionSettings();
  const captureSettings = readCaptureForm(base);

  await saveAppearance();

  const next = await sendMessage<PopupStateResponse>({
    type: 'extension_settings_update',
    settings: captureSettings,
  });

  if (next.appearanceSettings) fillAppearanceForm(next.appearanceSettings);
  if (next.extensionSettings) fillCaptureForm(next.extensionSettings);

  if (status) {
    status.textContent = next.connection === 'connected'
      ? 'Saved. Capture settings synced to desktop; appearance is local only.'
      : 'Saved locally (desktop not connected). Appearance is local only.';
  }
});

buildSwatches();

void (async () => {
  const state = await sendMessage<PopupStateResponse>({ type: 'popup_get_state' });
  fillAppearanceForm(state.appearanceSettings ?? DEFAULT_APPEARANCE_SETTINGS);
  fillCaptureForm(state.extensionSettings ?? createDefaultExtensionSettings());

  // Soft refresh connection / capture sync without overwriting local appearance.
  const refreshed = await sendMessage<PopupStateResponse>({ type: 'popup_ping' });
  if (refreshed.extensionSettings) fillCaptureForm(refreshed.extensionSettings);
  // Re-apply local appearance in case anything else touched CSS vars.
  if (!appearanceDirty) {
    fillAppearanceForm(refreshed.appearanceSettings ?? draftAppearance);
  } else {
    applyExtensionAppearance(draftAppearance);
  }
})();
