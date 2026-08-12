import {
  DEFAULT_CAPTURED_FILE_EXTENSIONS,
  normalizeExcludedHostPattern,
  type ExtensionIntegrationSettings,
} from '@rusticdl/protocol';
import browser from 'webextension-polyfill';
import type { PopupRequest, PopupStateResponse } from '../shared/messages';
import { createDefaultExtensionSettings } from '../shared/defaultExtensionSettings';
import {
  APPEARANCE_STORAGE_KEY,
  applyExtensionAppearance,
  DEFAULT_APPEARANCE_SETTINGS,
  normalizeAppearanceSettings,
  readCachedAppearance,
} from '../shared/appearance';
import { normalizeFileExtensionTag, TagListController } from '../shared/tagList';

const enabled = document.querySelector<HTMLInputElement>('#enabled');
const contextMenu = document.querySelector<HTMLInputElement>('#context-menu');
const badge = document.querySelector<HTMLInputElement>('#badge');
const silent = document.querySelector<HTMLInputElement>('#silent');
const saveButton = document.querySelector<HTMLButtonElement>('#save');
const status = document.querySelector<HTMLElement>('#status');
const connectionPill = document.querySelector<HTMLElement>('#connection-pill');
const connectionLabel = document.querySelector<HTMLElement>('#connection-label');
const extsReset = document.querySelector<HTMLButtonElement>('#exts-reset');

// Immediate paint from last-known desktop appearance.
applyExtensionAppearance(readCachedAppearance());

const hostsList = new TagListController({
  listEl: document.querySelector<HTMLElement>('#hosts-list')!,
  inputEl: document.querySelector<HTMLInputElement>('#hosts-input')!,
  addButton: document.querySelector<HTMLButtonElement>('#hosts-add'),
  filterEl: document.querySelector<HTMLInputElement>('#hosts-filter'),
  countEl: document.querySelector<HTMLElement>('#hosts-count'),
  feedbackEl: document.querySelector<HTMLElement>('#hosts-feedback'),
  emptyEl: document.querySelector<HTMLElement>('#hosts-empty'),
  itemNoun: 'host',
  emptyLabel: 'No excluded hosts. All matching downloads are captured.',
  normalize: normalizeExcludedHostPattern,
  invalidMessage: (raw) =>
    `"${raw}" is not a valid host pattern. Use a hostname like example.com or *.cdn.example.com.`,
  duplicateMessage: (value) => `"${value}" is already excluded.`,
});

const extsList = new TagListController({
  listEl: document.querySelector<HTMLElement>('#exts-list')!,
  inputEl: document.querySelector<HTMLInputElement>('#exts-input')!,
  addButton: document.querySelector<HTMLButtonElement>('#exts-add'),
  filterEl: document.querySelector<HTMLInputElement>('#exts-filter'),
  countEl: document.querySelector<HTMLElement>('#exts-count'),
  feedbackEl: document.querySelector<HTMLElement>('#exts-feedback'),
  emptyEl: document.querySelector<HTMLElement>('#exts-empty'),
  itemNoun: 'extension',
  emptyLabel: 'No extensions listed. Capture will rely on MIME hints only.',
  normalize: normalizeFileExtensionTag,
  invalidMessage: (raw) =>
    `"${raw}" is not a valid extension. Use letters/numbers only (e.g. zip, 7z, pdf).`,
  duplicateMessage: (value) => `".${value}" is already in the list.`,
});

async function sendMessage<T>(message: PopupRequest): Promise<T> {
  return browser.runtime.sendMessage(message) as Promise<T>;
}

function connectionText(connection: PopupStateResponse['connection']): string {
  switch (connection) {
    case 'connected':
      return 'Connected to RusticDL — appearance follows the app';
    case 'host_missing':
      return 'Backend not registered — using last known appearance';
    case 'app_missing':
      return 'RusticDL not installed — using last known appearance';
    case 'app_unreachable':
      return 'RusticDL unreachable — using last known appearance';
    case 'error':
      return 'Connection error — using last known appearance';
    default:
      return 'Checking connection…';
  }
}

function renderConnection(state: PopupStateResponse) {
  if (connectionPill) {
    connectionPill.className = `connection-pill ${state.connection}`;
  }
  if (connectionLabel) {
    connectionLabel.textContent = connectionText(state.connection);
  }
}

function fillCaptureForm(settings: ExtensionIntegrationSettings) {
  if (enabled) enabled.checked = settings.enabled;
  if (contextMenu) contextMenu.checked = settings.contextMenuEnabled;
  if (badge) badge.checked = settings.showBadgeStatus;
  if (silent) silent.checked = settings.downloadHandoffMode === 'auto';
  hostsList.setValues(settings.excludedHosts);
  extsList.setValues(settings.capturedFileExtensions);
}

function readCaptureForm(base: ExtensionIntegrationSettings): ExtensionIntegrationSettings {
  return {
    ...base,
    enabled: enabled?.checked ?? base.enabled,
    contextMenuEnabled: contextMenu?.checked ?? base.contextMenuEnabled,
    showBadgeStatus: badge?.checked ?? base.showBadgeStatus,
    downloadHandoffMode: silent?.checked ? 'auto' : 'ask',
    excludedHosts: hostsList.getValues(),
    capturedFileExtensions: extsList.getValues(),
  };
}

function applyAppearanceFromState(state: PopupStateResponse) {
  applyExtensionAppearance(state.appearanceSettings ?? DEFAULT_APPEARANCE_SETTINGS);
}

extsReset?.addEventListener('click', () => {
  extsList.setValues([...DEFAULT_CAPTURED_FILE_EXTENSIONS]);
  const feedback = document.querySelector('#exts-feedback');
  if (feedback) feedback.textContent = 'Restored default file extensions (not saved yet).';
});

saveButton?.addEventListener('click', async () => {
  if (status) status.textContent = 'Saving…';
  if (saveButton) saveButton.disabled = true;

  try {
    const state = await sendMessage<PopupStateResponse>({ type: 'popup_get_state' });
    const base = state.extensionSettings ?? createDefaultExtensionSettings();
    const captureSettings = readCaptureForm(base);

    const next = await sendMessage<PopupStateResponse>({
      type: 'extension_settings_update',
      settings: captureSettings,
    });

    applyAppearanceFromState(next);
    renderConnection(next);
    if (next.extensionSettings) fillCaptureForm(next.extensionSettings);

    if (status) {
      status.textContent = next.connection === 'connected'
        ? 'Saved and synced to RusticDL.'
        : 'Saved locally (desktop not connected). Will sync when RusticDL is available.';
    }
  } catch (error) {
    if (status) {
      status.textContent = error instanceof Error
        ? `Save failed: ${error.message}`
        : 'Save failed.';
    }
  } finally {
    if (saveButton) saveButton.disabled = false;
  }
});

// Background may refresh appearance after connection health pings — no extra polling.
browser.storage.onChanged.addListener((changes, area) => {
  if (area !== 'local') return;
  const appearanceChange = changes[APPEARANCE_STORAGE_KEY];
  if (appearanceChange?.newValue) {
    applyExtensionAppearance(normalizeAppearanceSettings(appearanceChange.newValue));
  }
});

void (async () => {
  const state = await sendMessage<PopupStateResponse>({ type: 'popup_get_state' });
  applyAppearanceFromState(state);
  renderConnection(state);
  fillCaptureForm(state.extensionSettings ?? createDefaultExtensionSettings());

  // Soft refresh pulls capture + appearance from desktop when available.
  const refreshed = await sendMessage<PopupStateResponse>({ type: 'popup_ping' });
  applyAppearanceFromState(refreshed);
  renderConnection(refreshed);
  if (refreshed.extensionSettings) fillCaptureForm(refreshed.extensionSettings);
})();
