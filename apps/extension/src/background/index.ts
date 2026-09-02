import {
  isErrorResponse,
  toUserFacingMessage,
  type ExtensionIntegrationSettings,
  type HostToExtensionResponse,
} from '@rusticdl/protocol';
import browser from './browser';
import {
  captureSessionsReady,
  configureCaptureCoordinator,
  flushQueuedCaptureEvents,
  followCaptureRedirect,
  handleFirefoxBeforeRequestSync,
  handleFirefoxHeadersReceivedSync,
  hydrateCaptureSessions,
  onChromiumBeforeSendHeaders,
  onChromiumDeterminingFilename,
  onChromiumHeadersReceived,
  onDownloadChanged,
  onDownloadCreated,
  pauseIfLikelyCapture,
} from './captureCoordinator';
import {
  getChromiumDeterminingFilenameApi,
  getChromiumWebRequest,
} from './chromiumCapture';
import {
  getFirefoxBlockingWebRequest,
  type FirefoxBeforeRequestDetails,
  type FirefoxHeadersReceivedDetails,
} from './firefoxCapture';
import {
  buildContextMenuPayload,
  collectHandoffAuth,
  connectionForErrorCode,
  handoffDownload,
  openApp,
  pingNativeHost,
  saveExtensionSettings,
} from './nativeMessaging';
import {
  getAppearanceSettings,
  getExtensionSettings,
  getPopupState,
  setExtensionSettings,
  setHostError,
  setLastResult,
  updatePopupState,
} from './state';
import type { PopupRequest, PopupStateResponse } from '../shared/messages';

const CONTEXT_MENU_ID = 'download-with-rusticdl';
/** Periodic desktop connection / queue badge refresh (not appearance sync). */
const CONNECTION_HEALTH_ALARM_NAME = 'connection-health';

let cachedSettings: ExtensionIntegrationSettings | null = null;

/** Blocking listeners stay sync. Null until storage loads so we do not override Off/ignore. */
function settingsForSyncCapture(): ExtensionIntegrationSettings | null {
  return cachedSettings;
}

async function getCachedSettings(): Promise<ExtensionIntegrationSettings> {
  if (!cachedSettings) {
    cachedSettings = await getExtensionSettings();
  }
  return cachedSettings;
}

function rememberSettings(settings: ExtensionIntegrationSettings): ExtensionIntegrationSettings {
  cachedSettings = settings;
  return settings;
}

const CONTEXT_MENU_PROPS = {
  id: CONTEXT_MENU_ID,
  title: 'Download with RusticDL',
  contexts: ['link'] as ['link'],
};

type ChromeRuntime = {
  lastError?: { message?: string };
};

type ChromeContextMenus = {
  create: (
    createProperties: typeof CONTEXT_MENU_PROPS,
    callback?: () => void,
  ) => string | number;
};

function chromeContextMenus(): ChromeContextMenus | undefined {
  return (globalThis as { chrome?: { contextMenus?: ChromeContextMenus } }).chrome?.contextMenus;
}

function chromeLastErrorMessage(): string | undefined {
  return (globalThis as { chrome?: { runtime?: ChromeRuntime } }).chrome?.runtime?.lastError?.message;
}

function isDuplicateMenuError(message: string | undefined): boolean {
  return Boolean(message && /duplicate id/i.test(message));
}

/** One rebuild at a time — overlapping create() calls trip Chromium's lastError. */
let contextMenuTask = Promise.resolve();

function ensureContextMenu(): Promise<void> {
  contextMenuTask = contextMenuTask.then(rebuildContextMenu, rebuildContextMenu);
  return contextMenuTask;
}

async function rebuildContextMenu(): Promise<void> {
  const settings = await getCachedSettings();
  if (!settings.contextMenuEnabled) {
    await browser.contextMenus.removeAll();
    return;
  }

  try {
    await browser.contextMenus.update(CONTEXT_MENU_ID, {
      title: CONTEXT_MENU_PROPS.title,
      contexts: CONTEXT_MENU_PROPS.contexts,
    });
    return;
  } catch {
    // Missing item: first install, or Chromium dropped persisted menus.
  }

  try {
    await createContextMenuItem();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!isDuplicateMenuError(message)) {
      console.warn('[RusticDL] context menu create failed', error);
    }
  }
}

/**
 * webextension-polyfill does not wrap contextMenus.create. Chromium's API is
 * callback-only and logs "Unchecked runtime.lastError" unless lastError is read.
 */
function createContextMenuItem(): Promise<void> {
  const chromeMenus = chromeContextMenus();
  if (chromeMenus) {
    return new Promise((resolve, reject) => {
      chromeMenus.create(CONTEXT_MENU_PROPS, () => {
        const message = chromeLastErrorMessage();
        if (message && !isDuplicateMenuError(message)) {
          reject(new Error(message));
          return;
        }
        resolve();
      });
    });
  }

  return Promise.resolve(
    browser.contextMenus.create(CONTEXT_MENU_PROPS) as Promise<string | number> | string | number,
  ).then(() => undefined);
}

async function refreshConnectionState(): Promise<HostToExtensionResponse> {
  const response = await pingNativeHost();
  if (isErrorResponse(response)) {
    const connection = connectionForErrorCode(response.code);
    const state = await setHostError(
      response.code,
      response.message || toUserFacingMessage(response.code, response.message),
      connection,
    );
    await updateBrowserBadge(state);
    return response;
  }

  const previousMenuEnabled = cachedSettings?.contextMenuEnabled;
  const state = await setLastResult('connected', response);
  if (state.extensionSettings) {
    rememberSettings(state.extensionSettings);
  }
  if (state.extensionSettings?.contextMenuEnabled !== previousMenuEnabled) {
    await ensureContextMenu();
  }
  await updateBrowserBadge(state);
  return response;
}

async function updateBrowserBadge(state: PopupStateResponse) {
  const settings = state.extensionSettings ?? (await getCachedSettings());
  const action = browser.action ?? browser.browserAction;
  if (!action?.setBadgeText) return;

  if (!settings.showBadgeStatus) {
    await action.setBadgeText({ text: '' });
    return;
  }

  if (state.connection !== 'connected') {
    await action.setBadgeText({ text: '!' });
    await action.setBadgeBackgroundColor?.({ color: '#b45309' });
    return;
  }

  const active = state.queueSummary?.active ?? 0;
  await action.setBadgeText({ text: active > 0 ? String(Math.min(active, 99)) : '' });
  await action.setBadgeBackgroundColor?.({ color: '#2563eb' });
}

configureCaptureCoordinator({ updateBadge: updateBrowserBadge });

function registerChromiumFilenameHints(): void {
  const webRequest = getChromiumWebRequest();
  webRequest?.onHeadersReceived?.addListener(
    (details) => onChromiumHeadersReceived(details, settingsForSyncCapture()),
    { urls: ['http://*/*', 'https://*/*'] },
    ['responseHeaders'],
  );
  if (webRequest?.onBeforeSendHeaders) {
    const filter = { urls: ['http://*/*', 'https://*/*'] };
    try {
      webRequest.onBeforeSendHeaders.addListener(
        onChromiumBeforeSendHeaders,
        filter,
        ['requestHeaders', 'extraHeaders'],
      );
    } catch {
      try {
        webRequest.onBeforeSendHeaders.addListener(
          onChromiumBeforeSendHeaders,
          filter,
          ['requestHeaders'],
        );
      } catch {
        // Observing request auth is optional; cookies.getAll is the fallback.
      }
    }
  }
  webRequest?.onBeforeRedirect?.addListener(
    (details) => {
      if (!details.redirectUrl) return;
      followCaptureRedirect(details.url, details.redirectUrl);
    },
    { urls: ['http://*/*', 'https://*/*'] },
  );

  const determining = getChromiumDeterminingFilenameApi();
  determining?.addListener((item, suggest) => {
    onChromiumDeterminingFilename(item, suggest, settingsForSyncCapture());
  });
}

function registerFirefoxWebRequestInterception() {
  const webRequest = getFirefoxBlockingWebRequest();
  if (!webRequest) return;

  const filter = {
    urls: ['http://*/*', 'https://*/*'],
    // xmlhttprequest is included so file-host CDNs (Gofile/Pixeldrain fetch)
    // can be handed off before they become an uncatchable blob: download.
    types: ['main_frame', 'object', 'other', 'xmlhttprequest'],
  };

  webRequest.onBeforeRequest?.addListener(
    (details: FirefoxBeforeRequestDetails) =>
      handleFirefoxBeforeRequestSync(details, settingsForSyncCapture()),
    filter,
    ['blocking'],
  );

  webRequest.onHeadersReceived.addListener(
    (details: FirefoxHeadersReceivedDetails) =>
      handleFirefoxHeadersReceivedSync(details, webRequest, settingsForSyncCapture()),
    filter,
    ['blocking', 'responseHeaders'],
  );

  webRequest.onBeforeRedirect?.addListener(
    (details) => {
      if (!details.redirectUrl) return;
      followCaptureRedirect(details.url, details.redirectUrl, details.requestId);
    },
    { urls: ['http://*/*', 'https://*/*'] },
  );
}

browser.runtime.onInstalled.addListener(() => {
  void ensureContextMenu();
  void refreshConnectionState();
  void browser.alarms.create(CONNECTION_HEALTH_ALARM_NAME, { periodInMinutes: 1 });
  // Drop the legacy appearance-sync alarm from older builds.
  void browser.alarms.clear('appearance-sync');
});

browser.runtime.onStartup.addListener(() => {
  void ensureContextMenu();
  void refreshConnectionState();
});

browser.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === CONNECTION_HEALTH_ALARM_NAME || alarm.name === 'appearance-sync') {
    void refreshConnectionState();
  }
});

browser.contextMenus.onClicked.addListener((info, tab) => {
  void (async () => {
    const settings = await getCachedSettings();
    if (!settings.contextMenuEnabled) return;
    const payload = buildContextMenuPayload(info, tab);
    if (!payload) {
      await setHostError('INVALID_URL', 'The selected link did not include a URL.', 'error');
      return;
    }
    await updatePopupState({ isSubmitting: true });
    const mode = settings.downloadHandoffMode === 'auto' ? 'auto' : 'ask';
    const handoffAuth = await collectHandoffAuth(payload.url, {
      referrer: payload.source.referrer,
      pageUrl: payload.source.pageUrl,
      incognito: payload.source.incognito,
      cookieStoreId: (tab as { cookieStoreId?: string } | undefined)?.cookieStoreId,
    });
    const response = await handoffDownload(payload.url, payload.source, mode, { handoffAuth });
    if (isErrorResponse(response)) {
      const connection = connectionForErrorCode(response.code);
      const state = await setHostError(
        response.code,
        response.message || toUserFacingMessage(response.code, response.message),
        connection,
      );
      await updateBrowserBadge(state);
      return;
    }
    const state = await setLastResult('connected', response);
    await updateBrowserBadge(state);
  })();
});

function registerDownloadCaptureListeners(
  whenReady: Promise<ExtensionIntegrationSettings>,
): void {
  // Firefox primary: blocking webRequest. downloads.onCreated is fallback.
  registerFirefoxWebRequestInterception();
  // Chromium: observe Content-Disposition so handoff is not a URL token.
  registerChromiumFilenameHints();
  if (browser.downloads?.onCreated) {
    browser.downloads.onCreated.addListener((item) => {
      pauseIfLikelyCapture(item, settingsForSyncCapture());
      void whenReady.then((settings) => onDownloadCreated(item, settings));
    });
  }
  if (browser.downloads?.onChanged) {
    browser.downloads.onChanged.addListener((delta) => {
      const settings = settingsForSyncCapture();
      if (captureSessionsReady() && settings) {
        onDownloadChanged(delta, settings);
        return;
      }
      void whenReady.then((readySettings) => onDownloadChanged(delta, readySettings));
    });
  }
}

browser.runtime.onMessage.addListener((message: PopupRequest) => {
  return handlePopupMessage(message);
});

async function handlePopupMessage(message: PopupRequest): Promise<PopupStateResponse | void> {
  switch (message.type) {
    case 'popup_get_state':
      return getPopupState();
    case 'popup_ping': {
      await refreshConnectionState();
      return getPopupState();
    }
    case 'popup_open_app': {
      const response = await openApp();
      if (isErrorResponse(response)) {
        await setHostError(
          response.code,
          response.message || toUserFacingMessage(response.code, response.message),
          connectionForErrorCode(response.code),
        );
      } else {
        await setLastResult('connected', response);
      }
      return getPopupState();
    }
    case 'popup_open_options': {
      await browser.runtime.openOptionsPage();
      return getPopupState();
    }
    case 'extension_settings_update': {
      const settings = rememberSettings(await setExtensionSettings(message.settings));
      const response = await saveExtensionSettings(settings);
      if (!isErrorResponse(response) && response.ok && response.type === 'pong') {
        await setLastResult('connected', response);
      }
      await ensureContextMenu();
      return getPopupState();
    }
    case 'appearance_settings_get': {
      const appearanceSettings = await getAppearanceSettings();
      return updatePopupState({ appearanceSettings });
    }
    default: {
      const _exhaustive: never = message;
      return _exhaustive;
    }
  }
}

// Chromium MV3 only delivers the event that woke the worker if addListener
// ran in this turn. Start hydrate first (it yields on await), then register
// so the waking download is paused and replayed after sessions + settings
// load — without persisting an empty store over storage.
const whenCaptureReady = (async () => {
  const [, settings] = await Promise.all([
    hydrateCaptureSessions(),
    getCachedSettings(),
  ]);
  await flushQueuedCaptureEvents(settings);
  return settings;
})();
registerDownloadCaptureListeners(whenCaptureReady);
void whenCaptureReady.then(() => {
  void ensureContextMenu();
  void refreshConnectionState();
});
