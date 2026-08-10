import type {
  AppearanceSettings,
  ErrorCode,
  ExtensionIntegrationSettings,
  HostToExtensionResponse,
  PongPayload,
} from '@rusticdl/protocol';
import browser from './browser';
import type { PopupStateResponse } from '../shared/messages';
import {
  createDefaultExtensionSettings,
  normalizeExtensionSettings,
} from '../shared/defaultExtensionSettings';
import {
  APPEARANCE_STORAGE_KEY,
  DEFAULT_APPEARANCE_SETTINGS,
  normalizeAppearanceSettings,
} from '../shared/appearance';

const STATE_KEY = 'popup-state';
const EXTENSION_SETTINGS_KEY = 'extension-settings';

const defaultState: PopupStateResponse = {
  connection: 'checking',
  isSubmitting: false,
  extensionSettings: createDefaultExtensionSettings(),
  appearanceSettings: DEFAULT_APPEARANCE_SETTINGS,
  extensionVersion: readExtensionVersion(),
};

export async function getAppearanceSettings(): Promise<AppearanceSettings> {
  const stored = await browser.storage.local.get(APPEARANCE_STORAGE_KEY);
  const fromDedicated = stored[APPEARANCE_STORAGE_KEY] as Partial<AppearanceSettings> | undefined;
  if (fromDedicated) {
    return normalizeAppearanceSettings(fromDedicated);
  }

  // One-time migration: older builds kept appearance only inside popup-state.
  const popupStored = await browser.storage.local.get(STATE_KEY);
  const legacy = (popupStored[STATE_KEY] as Partial<PopupStateResponse> | undefined)
    ?.appearanceSettings;
  const migrated = normalizeAppearanceSettings(legacy);
  await browser.storage.local.set({ [APPEARANCE_STORAGE_KEY]: migrated });
  return migrated;
}

export async function setAppearanceSettings(
  settings: Partial<AppearanceSettings> | AppearanceSettings,
): Promise<AppearanceSettings> {
  const normalized = normalizeAppearanceSettings(settings);
  await browser.storage.local.set({ [APPEARANCE_STORAGE_KEY]: normalized });
  await updatePopupState({ appearanceSettings: normalized });
  return normalized;
}

export async function getPopupState(): Promise<PopupStateResponse> {
  const stored = await browser.storage.local.get(STATE_KEY);
  const state = { ...defaultState, ...(stored[STATE_KEY] as Partial<PopupStateResponse> | undefined) };
  const appearanceSettings = await getAppearanceSettings();
  return {
    ...state,
    extensionVersion: state.extensionVersion ?? readExtensionVersion(),
    // Always prefer the dedicated local store over anything cached in popup-state.
    appearanceSettings,
  };
}

export async function updatePopupState(
  update: Partial<PopupStateResponse>,
): Promise<PopupStateResponse> {
  const current = await getPopupState();
  const nextState: PopupStateResponse = {
    ...current,
    ...update,
    // Appearance is local-only; never leave it undefined.
    appearanceSettings: normalizeAppearanceSettings(
      update.appearanceSettings ?? current.appearanceSettings,
    ),
  };
  await browser.storage.local.set({ [STATE_KEY]: nextState });
  return nextState;
}

export async function getExtensionSettings(): Promise<ExtensionIntegrationSettings> {
  const stored = await browser.storage.local.get(EXTENSION_SETTINGS_KEY);
  return normalizeExtensionSettings(
    stored[EXTENSION_SETTINGS_KEY] as Partial<ExtensionIntegrationSettings> | undefined,
  );
}

export async function setExtensionSettings(
  settings: ExtensionIntegrationSettings,
): Promise<ExtensionIntegrationSettings> {
  const normalized = normalizeExtensionSettings(settings);
  await browser.storage.local.set({ [EXTENSION_SETTINGS_KEY]: normalized });
  await updatePopupState({ extensionSettings: normalized });
  return normalized;
}

export async function setHostError(
  code: ErrorCode,
  message: string,
  connection: PopupStateResponse['connection'],
) {
  const extensionSettings = await getExtensionSettings();
  return updatePopupState({
    connection,
    isSubmitting: false,
    extensionSettings,
    lastResult: undefined,
    lastError: { code, message },
  });
}

export async function setLastResult(
  connection: PopupStateResponse['connection'],
  response: HostToExtensionResponse,
) {
  const currentState = await getPopupState();
  const payload =
    response.ok && response.type === 'pong' ? (response.payload as PongPayload) : undefined;
  const extensionSettings = payload?.extensionSettings
    ? await setExtensionSettings(payload.extensionSettings)
    : await getExtensionSettings();

  // Intentionally ignore payload.appearanceSettings — extension theme/accent
  // are owned by the extension, not mirrored from the desktop app.
  const appearanceSettings = await getAppearanceSettings();

  return updatePopupState({
    connection: payload?.connectionState ?? connection,
    isSubmitting: false,
    queueSummary: payload?.queueSummary ?? currentState.queueSummary,
    extensionSettings,
    appearanceSettings,
    desktopAppVersion: payload?.appVersion ?? currentState.desktopAppVersion,
    lastResult: response,
    lastError: response.ok ? undefined : { code: response.code, message: response.message },
  });
}

function readExtensionVersion(): string {
  try {
    return browser.runtime.getManifest().version || '0.0.0';
  } catch {
    return '0.0.0';
  }
}

export type { AppearanceSettings };
