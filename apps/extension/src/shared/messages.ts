import type {
  AppearanceSettings,
  ErrorCode,
  ExtensionIntegrationSettings,
  HostToExtensionResponse,
  QueueSummary,
} from '@rusticdl/protocol';

export type PopupRequest =
  | { type: 'popup_ping' }
  | { type: 'popup_get_state' }
  | { type: 'popup_open_app' }
  | { type: 'popup_open_options' }
  | { type: 'extension_settings_update'; settings: ExtensionIntegrationSettings }
  | { type: 'appearance_settings_get' };

export interface PopupStateResponse {
  connection: 'checking' | 'connected' | 'host_missing' | 'app_missing' | 'app_unreachable' | 'error';
  isSubmitting: boolean;
  queueSummary?: QueueSummary;
  extensionSettings?: ExtensionIntegrationSettings;
  /** Mirrors desktop appearance (cached for offline / FOUC paint). */
  appearanceSettings?: AppearanceSettings;
  lastResult?: HostToExtensionResponse;
  lastError?: { code: ErrorCode; message: string };
  desktopAppVersion?: string;
  extensionVersion?: string;
}
