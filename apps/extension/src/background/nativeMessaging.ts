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
  type HandoffAuthHeader,
  type HostToExtensionResponse,
  type OriginHandoffAuth,
  type RequestSource,
} from '@rusticdl/protocol';
import {
  cookieStoreIdForHandoff,
  cookieUrlsForHandoff,
  httpOrigin,
} from './captureFilter';
import browser from './browser';
import type { PopupStateResponse } from '../shared/messages';

function browserLabel(): string {
  switch (detectBrowser()) {
    case 'firefox':
      return 'Firefox';
    case 'edge':
      return 'Edge';
    default:
      return 'Chrome';
  }
}

function mapNativeMessagingError(error: unknown): {
  code: ErrorCode;
  message: string;
  connection: PopupStateResponse['connection'];
} {
  const message = error instanceof Error ? error.message : 'Native messaging failed.';
  const lowered = message.toLowerCase();

  if (lowered.includes('forbidden')) {
    return {
      code: 'HOST_REGISTRATION_MISSING',
      message:
        `This ${browserLabel()} extension is not allowed to use RusticDL Backend. `
        + 'The host is registered, but this extension id is missing from allowed_origins. '
        + 'Reload the unpacked extension after rebuilding, or from the repo root run:\n'
        + '.\\scripts\\register-native-host.ps1',
      connection: 'host_missing',
    };
  }

  if (
    (lowered.includes('host') && lowered.includes('not found'))
    || lowered.includes('specified native messaging host not found')
    || lowered.includes('no such native application')
    || (lowered.includes('native application') && lowered.includes('not found'))
  ) {
    return {
      code: 'HOST_REGISTRATION_MISSING',
      message:
        `RusticDL Backend is not registered for ${browserLabel()}. From the repo root run:\n`
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

function mergeOriginHeaders(
  byOrigin: Map<string, HandoffAuthHeader[]>,
  origin: string,
  incoming: HandoffAuthHeader[],
): void {
  const existing = byOrigin.get(origin) ?? [];
  const names = new Set(existing.map((header) => header.name.toLowerCase()));
  for (const header of incoming) {
    if (!header.name || !header.value || names.has(header.name.toLowerCase())) continue;
    existing.push(header);
    names.add(header.name.toLowerCase());
  }
  if (existing.length > 0) {
    byOrigin.set(origin, existing);
  }
}

/**
 * Browser session headers so the desktop GET can replay the same file the
 * tab just requested. Without cookies/referer, file hosts return a 3 KB HTML
 * wait page that still has `filename="Game.rar"`.
 *
 * Collect cookies for every origin on the capture (Canvas file URL and the
 * Drive/Inst-FS hop). Do not log header values.
 */
export async function collectHandoffAuth(
  url: string,
  extra: {
    referrer?: string;
    pageUrl?: string;
    finalUrl?: string;
    incognito?: boolean;
    cookieStoreId?: string;
    originAuth?: OriginHandoffAuth[];
  } = {},
): Promise<DownloadRequestMetadata['handoffAuth']> {
  const headers: HandoffAuthHeader[] = [];
  const byOrigin = new Map<string, HandoffAuthHeader[]>();

  for (const entry of extra.originAuth ?? []) {
    const origin = httpOrigin(entry.origin) ?? httpOrigin(`${entry.origin}/`);
    if (!origin || !entry.headers?.length) continue;
    mergeOriginHeaders(byOrigin, origin, entry.headers);
  }

  try {
    let storeId = extra.cookieStoreId;
    if (!storeId) {
      try {
        const stores = await browser.cookies.getAllCookieStores();
        storeId = cookieStoreIdForHandoff(stores, extra);
      } catch {
        // Chromium may omit getAllCookieStores in some contexts.
      }
    }
    for (const cookieUrl of cookieUrlsForHandoff([
      url,
      extra.finalUrl,
      extra.referrer,
      extra.pageUrl,
    ])) {
      let cookies = await browser.cookies.getAll(storeId ? { url: cookieUrl, storeId } : { url: cookieUrl });
      // Chromium MV3 can return [] when storeId is guessed ("0"). Retry unbound.
      if (cookies.length === 0 && storeId) {
        cookies = await browser.cookies.getAll({ url: cookieUrl });
      }
      if (cookies.length === 0) continue;
      const origin = httpOrigin(cookieUrl);
      if (!origin) continue;
      mergeOriginHeaders(byOrigin, origin, [{
        name: 'Cookie',
        value: cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join('; '),
      }]);
    }
  } catch {
    // Restricted cookie / missing permission — still send referer/UA.
  }

  const primaryOrigin = httpOrigin(url);
  const primaryCookie = primaryOrigin
    ? byOrigin.get(primaryOrigin)?.find((header) => header.name.toLowerCase() === 'cookie')
    : undefined;
  if (primaryCookie) {
    headers.push(primaryCookie);
  }

  const referrer = extra.referrer || extra.pageUrl;
  if (referrer) {
    headers.push({ name: 'Referer', value: referrer });
    try {
      headers.push({ name: 'Origin', value: new URL(referrer).origin });
    } catch {
      // ignore invalid referrer
    }
  }

  if (typeof navigator !== 'undefined' && navigator.userAgent) {
    headers.push({ name: 'User-Agent', value: navigator.userAgent });
  }

  const originAuth = [...byOrigin.entries()].map(([origin, originHeaders]) => ({
    origin,
    headers: originHeaders,
  }));
  if (headers.length === 0 && originAuth.length === 0) return undefined;
  return {
    headers,
    ...(originAuth.length > 0 ? { originAuth } : {}),
  };
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
