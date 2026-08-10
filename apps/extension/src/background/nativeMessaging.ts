import {
  HOST_NAME,
  createEnqueueDownloadRequest,
  createOpenAppRequest,
  createPingRequest,
  createPromptDownloadRequest,
  createSaveExtensionSettingsRequest,
  toUserFacingMessage,
  type BrowserKind,
  type DownloadRequestMetadata,
  type EnqueueDownloadPayload,
  type ErrorCode,
  type ExtensionIntegrationSettings,
  type HostToExtensionResponse,
  type RequestSource,
} from '@rusticdl/protocol';
import browser from './browser';
import type { PopupStateResponse } from '../shared/messages';

function mapNativeMessagingError(error: unknown): {
  code: ErrorCode;
  message: string;
  connection: PopupStateResponse['connection'];
} {
  const message = error instanceof Error ? error.message : 'Native messaging failed.';
  const lowered = message.toLowerCase();

  if (
    (lowered.includes('host') && lowered.includes('not found'))
    || lowered.includes('specified native messaging host not found')
    || lowered.includes('no such native application')
    || lowered.includes('native application') && lowered.includes('not found')
  ) {
    return {
      code: 'HOST_REGISTRATION_MISSING',
      message:
        'Native host is not registered for Firefox. From the repo root run:\n'
        + '.\\scripts\\register-native-host.ps1 -HostBinaryPath "$PWD\\target\\debug\\rusticdl-native-host.exe"',
      connection: 'host_missing',
    };
  }

  return {
    code: 'HOST_NOT_AVAILABLE',
    message: toUserFacingMessage('HOST_NOT_AVAILABLE', message),
    connection: 'error',
  };
}

export function connectionForErrorCode(code: ErrorCode): PopupStateResponse['connection'] {
  switch (code) {
    case 'HOST_REGISTRATION_MISSING':
      return 'host_missing';
    case 'APP_NOT_INSTALLED':
      return 'app_missing';
    case 'APP_UNREACHABLE':
      return 'app_unreachable';
    default:
      return 'error';
  }
}

async function sendNativeMessage(request: object): Promise<HostToExtensionResponse> {
  try {
    return (await browser.runtime.sendNativeMessage(HOST_NAME, request)) as HostToExtensionResponse;
  } catch (error) {
    const mapped = mapNativeMessagingError(error);
    return {
      ok: false,
      requestId: 'native_messaging_error',
      type: 'rejected',
      code: mapped.code,
      message: mapped.message,
    };
  }
}

export function detectBrowser(): BrowserKind {
  const userAgent = navigator.userAgent.toLowerCase();
  if (userAgent.includes('firefox')) return 'firefox';
  if (userAgent.includes('edg/')) return 'edge';
  return 'chrome';
}

export async function pingNativeHost(): Promise<HostToExtensionResponse> {
  return sendNativeMessage(createPingRequest());
}

export async function openApp(): Promise<HostToExtensionResponse> {
  return sendNativeMessage(createOpenAppRequest({ reason: 'user_request' }));
}

export async function enqueueDownload(
  url: string,
  source: Omit<RequestSource, 'browser'>,
  metadata: DownloadRequestMetadata = {},
): Promise<HostToExtensionResponse> {
  const request = createEnqueueDownloadRequest(
    url,
    { ...source, browser: detectBrowser() },
    undefined,
    metadata,
  );
  if (!request.ok) {
    return {
      ok: false,
      requestId: 'validation_error',
      type: 'rejected',
      code: request.code,
      message: request.message,
    };
  }
  return sendNativeMessage(request.value);
}

export async function promptDownload(
  url: string,
  source: Omit<RequestSource, 'browser'>,
  metadata: DownloadRequestMetadata = {},
): Promise<HostToExtensionResponse> {
  const request = createPromptDownloadRequest(url, { ...source, browser: detectBrowser() }, metadata);
  if (!request.ok) {
    return {
      ok: false,
      requestId: 'validation_error',
      type: 'rejected',
      code: request.code,
      message: request.message,
    };
  }
  return sendNativeMessage(request.value);
}

/**
 * Manual + automatic handoffs share the same ask/auto policy.
 * - auto → enqueue immediately
 * - ask (or off for explicit user actions) → desktop confirm dialog
 */
export async function handoffDownload(
  url: string,
  source: Omit<RequestSource, 'browser'>,
  mode: ExtensionIntegrationSettings['downloadHandoffMode'],
  metadata: DownloadRequestMetadata = {},
): Promise<HostToExtensionResponse> {
  if (mode === 'auto') {
    return enqueueDownload(url, source, metadata);
  }
  return promptDownload(url, source, metadata);
}

export async function saveExtensionSettings(
  settings: ExtensionIntegrationSettings,
): Promise<HostToExtensionResponse> {
  return sendNativeMessage(createSaveExtensionSettingsRequest(settings));
}

export function buildContextMenuPayload(
  info: browser.menus.OnClickData,
  tab?: browser.tabs.Tab,
): EnqueueDownloadPayload | null {
  if (!info.linkUrl) return null;
  return {
    url: info.linkUrl,
    source: {
      entryPoint: 'context_menu',
      browser: detectBrowser(),
      extensionVersion: browser.runtime.getManifest().version,
      pageUrl: tab?.url,
      pageTitle: tab?.title,
      referrer: tab?.url,
      incognito: tab?.incognito,
    },
  };
}
