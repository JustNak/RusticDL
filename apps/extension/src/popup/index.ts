import type { ExtensionIntegrationSettings } from '@rusticdl/protocol';
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

const connectionStatusDot = document.querySelector<HTMLSpanElement>('#connection-status');
const connectionStatusLabel = document.querySelector<HTMLSpanElement>('#connection-status-label');
const openAppButton = document.querySelector<HTMLButtonElement>('#open-app-button');
const syncButton = document.querySelector<HTMLButtonElement>('#sync-button');
const silentDownloadToggle = document.querySelector<HTMLInputElement>('#silent-download-toggle');
const extensionEnabledToggle = document.querySelector<HTMLInputElement>('#extension-enabled-toggle');
const advancedButton = document.querySelector<HTMLButtonElement>('#advanced-button');
const statusLine = document.querySelector<HTMLElement>('#status-line');

let currentState: PopupStateResponse | null = null;
let isUpdating = false;

// Paint immediately from last-known desktop appearance (FOUC cache).
applyExtensionAppearance(readCachedAppearance());

async function sendMessage<T>(message: PopupRequest): Promise<T> {
  return browser.runtime.sendMessage(message) as Promise<T>;
}

function connectionLabel(connection: PopupStateResponse['connection']): string {
  switch (connection) {
    case 'connected':
      return 'Connected to RusticDL';
    case 'host_missing':
      return 'RusticDL Backend not registered';
    case 'app_missing':
      return 'RusticDL not installed';
    case 'app_unreachable':
      return 'RusticDL unreachable — start the app';
    case 'error':
      return 'Connection error';
    default:
      return 'Checking connection';
  }
}

function renderState(state: PopupStateResponse) {
  currentState = state;
  const settings = state.extensionSettings ?? createDefaultExtensionSettings();

  // Appearance comes from the desktop app (cached when offline).
  applyExtensionAppearance(state.appearanceSettings ?? DEFAULT_APPEARANCE_SETTINGS);

  if (connectionStatusDot) {
    connectionStatusDot.className = `connection-dot ${state.connection}`;
  }
  if (connectionStatusLabel) {
    connectionStatusLabel.textContent = connectionLabel(state.connection);
  }
  if (statusLine) {
    if (state.lastError) {
      statusLine.textContent = state.lastError.message;
    } else {
      const version = state.desktopAppVersion ? ` · app ${state.desktopAppVersion}` : '';
      const queue = state.queueSummary
        ? ` · ${state.queueSummary.active} active / ${state.queueSummary.total} total`
        : '';
      statusLine.textContent = `${connectionLabel(state.connection)}${version}${queue}`;
    }
  }

  if (silentDownloadToggle) {
    silentDownloadToggle.checked = settings.downloadHandoffMode === 'auto';
    silentDownloadToggle.disabled = isUpdating || settings.enabled === false;
  }
  if (extensionEnabledToggle) {
    extensionEnabledToggle.checked = settings.enabled !== false;
    extensionEnabledToggle.disabled = isUpdating;
  }
  if (syncButton) syncButton.disabled = isUpdating;
  if (advancedButton) advancedButton.disabled = isUpdating;
  if (openAppButton) {
    // Offer one-click recovery when the desktop is not connected.
    const needsOpen = state.connection !== 'connected' && state.connection !== 'checking';
    openAppButton.hidden = !needsOpen;
    openAppButton.disabled = isUpdating;
  }
}

async function patchSettings(patch: Partial<ExtensionIntegrationSettings>) {
  if (!currentState?.extensionSettings) return;
  isUpdating = true;
  renderState(currentState);
  const settings = { ...currentState.extensionSettings, ...patch };
  const state = await sendMessage<PopupStateResponse>({
    type: 'extension_settings_update',
    settings,
  });
  isUpdating = false;
  renderState(state);
}

async function refresh() {
  isUpdating = true;
  if (currentState) renderState(currentState);
  const state = await sendMessage<PopupStateResponse>({ type: 'popup_ping' });
  isUpdating = false;
  renderState(state);
}

syncButton?.addEventListener('click', () => {
  void refresh();
});

openAppButton?.addEventListener('click', () => {
  void (async () => {
    isUpdating = true;
    if (currentState) renderState(currentState);
    const state = await sendMessage<PopupStateResponse>({ type: 'popup_open_app' });
    isUpdating = false;
    renderState(state);
  })();
});

advancedButton?.addEventListener('click', () => {
  void sendMessage({ type: 'popup_open_options' });
});

silentDownloadToggle?.addEventListener('change', () => {
  void patchSettings({
    downloadHandoffMode: silentDownloadToggle.checked ? 'auto' : 'ask',
  });
});

extensionEnabledToggle?.addEventListener('change', () => {
  void patchSettings({ enabled: extensionEnabledToggle.checked });
});

// Background may refresh appearance on connection health / ping — apply without polling.
browser.storage.onChanged.addListener((changes, area) => {
  if (area !== 'local') return;
  const appearanceChange = changes[APPEARANCE_STORAGE_KEY];
  if (appearanceChange?.newValue) {
    applyExtensionAppearance(normalizeAppearanceSettings(appearanceChange.newValue));
  }
});

void (async () => {
  const state = await sendMessage<PopupStateResponse>({ type: 'popup_get_state' });
  renderState(state);
  await refresh();
})();
