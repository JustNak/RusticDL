export const PROTOCOL_VERSION = 1;
export const HOST_NAME = 'com.rusticdl.native_host';
export const PIPE_NAME = '\\\\.\\pipe\\rusticdl.v1';
export const MAX_URL_LENGTH = 2048;
export const MAX_METADATA_LENGTH = 512;
export const ALLOWED_URL_PROTOCOLS = ['http:', 'https:'] as const;
export const DEFAULT_EXTENSION_EXCLUDED_HOSTS = ['web.telegram.org'] as const;
export const DEFAULT_CAPTURED_FILE_EXTENSIONS = [
  '7z', 'apk', 'bz2', 'cab', 'csv', 'deb', 'dmg', 'doc', 'docx', 'exe', 'gz', 'iso', 'jar',
  'msi', 'pdf', 'ppt', 'pptx', 'rar', 'rpm', 'tar', 'tgz', 'txz', 'xls', 'xlsx', 'xz', 'zip', 'zst',
] as const;

export type BrowserKind = 'chrome' | 'edge' | 'firefox';
export type ExtensionEntryPoint = 'context_menu' | 'popup' | 'browser_download';
export type DownloadHandoffMode = 'off' | 'ask' | 'auto';
export type AppearanceTheme = 'light' | 'dark' | 'system';
export type DesktopConnectionState =
  | 'checking'
  | 'connected'
  | 'host_missing'
  | 'app_missing'
  | 'app_unreachable'
  | 'error';

export type ErrorCode =
  | 'INVALID_PAYLOAD'
  | 'INVALID_URL'
  | 'UNSUPPORTED_SCHEME'
  | 'URL_TOO_LONG'
  | 'METADATA_TOO_LARGE'
  | 'HOST_NOT_AVAILABLE'
  | 'HOST_REGISTRATION_MISSING'
  | 'HOST_PROTOCOL_MISMATCH'
  | 'APP_NOT_INSTALLED'
  | 'APP_UNREACHABLE'
  | 'APP_TIMEOUT'
  | 'DESTINATION_NOT_CONFIGURED'
  | 'DESTINATION_INVALID'
  | 'DUPLICATE_JOB'
  | 'PERMISSION_DENIED'
  | 'RATE_LIMITED'
  | 'DOWNLOAD_FAILED'
  | 'INTERNAL_ERROR';

export interface RequestSource {
  entryPoint: ExtensionEntryPoint;
  browser: BrowserKind;
  extensionVersion: string;
  pageUrl?: string;
  pageTitle?: string;
  referrer?: string;
  incognito?: boolean;
}

export interface HandoffAuthHeader {
  name: string;
  value: string;
}

export interface HandoffAuth {
  headers: HandoffAuthHeader[];
}

export interface EnqueueDownloadPayload {
  url: string;
  source: RequestSource;
  suggestedFilename?: string;
  totalBytes?: number;
  handoffAuth?: HandoffAuth;
}

export interface PromptDownloadPayload {
  url: string;
  source: RequestSource;
  suggestedFilename?: string;
  totalBytes?: number;
  handoffAuth?: HandoffAuth;
}

export interface DownloadRequestMetadata {
  suggestedFilename?: string;
  totalBytes?: number;
  handoffAuth?: HandoffAuth;
}

export interface OpenAppPayload {
  reason: 'user_request' | 'reconnect';
}

export interface EmptyPayload {
  [key: string]: never;
}

export interface RequestEnvelope<TType extends string, TPayload> {
  protocolVersion: number;
  requestId: string;
  type: TType;
  payload: TPayload;
}

export interface SuccessResponse<TType extends string, TPayload> {
  ok: true;
  requestId: string;
  type: TType;
  payload: TPayload;
}

export interface ErrorResponse {
  ok: false;
  requestId: string;
  type: string;
  code: ErrorCode;
  message: string;
}

export interface AcceptedPayload {
  status: 'queued' | 'duplicate_existing_job' | 'dismissed';
  jobId?: string;
  filename?: string;
  appState: 'running' | 'launched';
}

export interface QueueSummary {
  total: number;
  active: number;
  attention: number;
  queued: number;
  downloading: number;
  completed: number;
  failed: number;
}

export interface AppearanceSettings {
  theme: AppearanceTheme;
  accentColor: string;
}

export interface ExtensionIntegrationSettings {
  enabled: boolean;
  downloadHandoffMode: DownloadHandoffMode;
  contextMenuEnabled: boolean;
  showProgressAfterHandoff: boolean;
  showBadgeStatus: boolean;
  excludedHosts: string[];
  ignoredFileExtensions: string[];
  capturedFileExtensions: string[];
  downloadCaptureDebugLogging: boolean;
}

export interface PongPayload {
  appState: 'running' | 'launched';
  appVersion?: string;
  connectionState?: DesktopConnectionState;
  queueSummary?: QueueSummary;
  extensionSettings?: ExtensionIntegrationSettings;
  appearanceSettings?: AppearanceSettings;
}

export type HostToExtensionResponse =
  | SuccessResponse<'pong', PongPayload>
  | SuccessResponse<'accepted', AcceptedPayload>
  | ErrorResponse;

export type ValidationResult<T> =
  | { ok: true; value: T }
  | { ok: false; code: ErrorCode; message: string };

export function createRequestId(): string {
  return crypto.randomUUID();
}

export function validateHttpUrl(input: string): ValidationResult<string> {
  if (!input.trim()) {
    return { ok: false, code: 'INVALID_URL', message: 'URL is required.' };
  }
  if (input.length > MAX_URL_LENGTH) {
    return { ok: false, code: 'URL_TOO_LONG', message: `URL exceeds ${MAX_URL_LENGTH} characters.` };
  }
  let parsed: URL;
  try {
    parsed = new URL(input);
  } catch {
    return { ok: false, code: 'INVALID_URL', message: 'URL is not valid.' };
  }
  if (!ALLOWED_URL_PROTOCOLS.includes(parsed.protocol as (typeof ALLOWED_URL_PROTOCOLS)[number])) {
    return { ok: false, code: 'UNSUPPORTED_SCHEME', message: 'Only http and https URLs are supported.' };
  }
  return { ok: true, value: parsed.toString() };
}

export function trimMetadata(value: string | undefined): string | undefined {
  if (!value) return undefined;
  return value.slice(0, MAX_METADATA_LENGTH);
}

export function sanitizeSource(source: RequestSource): RequestSource {
  return {
    entryPoint: source.entryPoint,
    browser: source.browser,
    extensionVersion: trimMetadata(source.extensionVersion) ?? '0.0.0',
    pageUrl: trimMetadata(source.pageUrl),
    pageTitle: trimMetadata(source.pageTitle),
    referrer: trimMetadata(source.referrer),
    incognito: source.incognito ?? false,
  };
}

export function normalizeExcludedHostPattern(value: string): string {
  let pattern = value
    .trim()
    .replace(/^https?:\/\//i, '')
    .replace(/^[^@/]+@/, '')
    .replace(/[/?#].*$/, '')
    .toLowerCase();
  pattern = pattern.replace(/:\d+$/, '');
  pattern = pattern.replace(/^\.+|\.+$/g, '');
  if (
    !pattern
    || pattern.includes('/')
    || pattern.includes('\\')
    || /\s/.test(pattern)
    || !/[a-z0-9]/.test(pattern)
    || !/^[a-z0-9.*-]+$/.test(pattern)
  ) {
    return '';
  }
  return pattern;
}

export function isHostnameExcludedByPatterns(hostname: string, patterns: string[]): boolean {
  const normalizedHostname = normalizeExcludedHostPattern(hostname);
  if (!normalizedHostname) return false;
  return patterns.some((pattern) => {
    const normalizedPattern = normalizeExcludedHostPattern(pattern);
    if (!normalizedPattern) return false;
    if (normalizedPattern.includes('*')) {
      const escaped = normalizedPattern
        .split('*')
        .map((part) => part.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&'))
        .join('[^.]*');
      return new RegExp(`^${escaped}$`).test(normalizedHostname);
    }
    return normalizedHostname === normalizedPattern || normalizedHostname.endsWith(`.${normalizedPattern}`);
  });
}

export function isUrlHostExcludedByPatterns(url: string, patterns: string[]): boolean {
  try {
    return isHostnameExcludedByPatterns(new URL(url).hostname, patterns);
  } catch {
    return false;
  }
}

function normalizeTotalBytes(totalBytes: number | undefined): number | undefined {
  return typeof totalBytes === 'number' && Number.isFinite(totalBytes) && totalBytes > 0
    ? Math.floor(totalBytes)
    : undefined;
}

function normalizeHandoffAuth(handoffAuth: HandoffAuth | undefined): HandoffAuth | undefined {
  if (!handoffAuth?.headers?.length) return undefined;
  const headers = handoffAuth.headers
    .map((header) => ({
      name: trimMetadata(header.name)?.trim() ?? '',
      value: header.value.slice(0, 16 * 1024),
    }))
    .filter((header) => header.name && header.value);
  return headers.length ? { headers } : undefined;
}

export function createPingRequest(requestId = createRequestId()): RequestEnvelope<'ping', EmptyPayload> {
  return { protocolVersion: PROTOCOL_VERSION, requestId, type: 'ping', payload: {} };
}

export function createGetStatusRequest(requestId = createRequestId()): RequestEnvelope<'get_status', EmptyPayload> {
  return { protocolVersion: PROTOCOL_VERSION, requestId, type: 'get_status', payload: {} };
}

export function createOpenAppRequest(
  payload: OpenAppPayload,
  requestId = createRequestId(),
): RequestEnvelope<'open_app', OpenAppPayload> {
  return { protocolVersion: PROTOCOL_VERSION, requestId, type: 'open_app', payload };
}

export function createEnqueueDownloadRequest(
  url: string,
  source: RequestSource,
  requestId = createRequestId(),
  metadata: DownloadRequestMetadata = {},
): ValidationResult<RequestEnvelope<'enqueue_download', EnqueueDownloadPayload>> {
  const validatedUrl = validateHttpUrl(url);
  if (!validatedUrl.ok) return validatedUrl;
  const handoffAuth = normalizeHandoffAuth(metadata.handoffAuth);
  return {
    ok: true,
    value: {
      protocolVersion: PROTOCOL_VERSION,
      requestId,
      type: 'enqueue_download',
      payload: {
        url: validatedUrl.value,
        source: sanitizeSource(source),
        suggestedFilename: trimMetadata(metadata.suggestedFilename),
        totalBytes: normalizeTotalBytes(metadata.totalBytes),
        ...(handoffAuth ? { handoffAuth } : {}),
      },
    },
  };
}

export function createPromptDownloadRequest(
  url: string,
  source: RequestSource,
  metadata: DownloadRequestMetadata = {},
  requestId = createRequestId(),
): ValidationResult<RequestEnvelope<'prompt_download', PromptDownloadPayload>> {
  const validatedUrl = validateHttpUrl(url);
  if (!validatedUrl.ok) return validatedUrl;
  const handoffAuth = normalizeHandoffAuth(metadata.handoffAuth);
  return {
    ok: true,
    value: {
      protocolVersion: PROTOCOL_VERSION,
      requestId,
      type: 'prompt_download',
      payload: {
        url: validatedUrl.value,
        source: sanitizeSource(source),
        suggestedFilename: trimMetadata(metadata.suggestedFilename),
        totalBytes: normalizeTotalBytes(metadata.totalBytes),
        ...(handoffAuth ? { handoffAuth } : {}),
      },
    },
  };
}

export function createSaveExtensionSettingsRequest(
  settings: ExtensionIntegrationSettings,
  requestId = createRequestId(),
): RequestEnvelope<'save_extension_settings', ExtensionIntegrationSettings> {
  return {
    protocolVersion: PROTOCOL_VERSION,
    requestId,
    type: 'save_extension_settings',
    payload: settings,
  };
}

export function isErrorResponse(response: HostToExtensionResponse): response is ErrorResponse {
  return !response.ok;
}

export function toUserFacingMessage(code: ErrorCode, fallback?: string): string {
  switch (code) {
    case 'HOST_REGISTRATION_MISSING':
      return 'RusticDL Backend is not registered. Run scripts/register-native-host.ps1, then reload the extension.';
    case 'APP_NOT_INSTALLED':
      return 'RusticDL was not found.';
    case 'APP_UNREACHABLE':
      return 'RusticDL is not responding. Launch RusticDL and try again.';
    case 'HOST_PROTOCOL_MISMATCH':
      return 'Extension and RusticDL protocol versions do not match.';
    case 'RATE_LIMITED':
      return 'Too many handoff requests. Wait a moment and try again.';
    default:
      return fallback || 'Could not complete the request.';
  }
}
