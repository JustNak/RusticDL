import type { ExtensionIntegrationSettings } from '@rusticdl/protocol';
import browser from 'webextension-polyfill';
import type { PopupRequest, PopupStateResponse } from '../shared/messages';
import { createDefaultExtensionSettings } from '../shared/defaultExtensionSettings';
import {
  applyExtensionAppearance,
  DEFAULT_APPEARANCE_SETTINGS,
  readCachedAppearance,
} from '../shared/appearance';

const connectionStatusDot = document.querySelector<HTMLSpanElement>('#connection-status');
const connectionStatusLabel = document.querySelector<HTMLSpanElement>('#connection-status-label');
const syncButton = document.querySelector<HTMLButtonElement>('#sync-button');
const silentDownloadToggle = document.querySelector<HTMLInputElement>('#silent-download-toggle');
const extensionEnabledToggle = document.querySelector<HTMLInputElement>('#extension-enabled-toggle');
const advancedButton = document.querySelector<HTMLButtonElement>('#advanced-button');
const openAppButton = document.querySelector<HTMLButtonElement>('#open-app-button');
const statusLine = document.querySelector<HTMLElement>('#status-line');

let currentState: PopupStateResponse | null = null;
let isUpdating = false;

// Paint immediately from local cache (no desktop dependency).
applyExtensionAppearance(readCachedAppearance());

async function sendMessage<T>(message: PopupRequest): Promise<T> {
  return browser.runtime.sendMessage(message) as Promise<T>;
}

function connectionLabel(connection: PopupStateResponse['connection']): string {
  switch (connection) {
    case 'connected':
      return 'Connected to desktop app';
    case 'host_missing':
      return 'Native host not registered';
    case 'app_missing':
      return 'Desktop app not installed';
    case 'app_unreachable':
      return 'Desktop app unreachable — start RusticDL';
    case 'error':
      return 'Connection error';
    default:
      return 'Checking connection';
  }
}

function renderState(state: PopupStateResponse) {
  currentState = state;
  const settings = state.extensionSettings ?? createDefaultExtensionSettings();

  // Always apply extension-local appearance, whether or not desktop is connected.
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
  if (openAppButton) openAppButton.disabled = isUpdating;
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

advancedButton?.addEventListener('click', () => {
  void sendMessage({ type: 'popup_open_options' });
});

openAppButton?.addEventListener('click', async () => {
  isUpdating = true;
  const state = await sendMessage<PopupStateResponse>({ type: 'popup_open_app' });
  isUpdating = false;
  renderState(state);
});

silentDownloadToggle?.addEventListener('change', () => {
  void patchSettings({
    downloadHandoffMode: silentDownloadToggle.checked ? 'auto' : 'ask',
  });
});

extensionEnabledToggle?.addEventListener('change', () => {
  void patchSettings({ enabled: extensionEnabledToggle.checked });
});

void (async () => {
  const state = await sendMessage<PopupStateResponse>({ type: 'popup_get_state' });
  renderState(state);
  await refresh();
})();
