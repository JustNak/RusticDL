import {
  DEFAULT_CAPTURED_FILE_EXTENSIONS,
  DEFAULT_EXTENSION_EXCLUDED_HOSTS,
  normalizeExcludedHostPattern,
  type ExtensionIntegrationSettings,
} from '@rusticdl/protocol';

export const defaultExtensionSettings: ExtensionIntegrationSettings = {
  enabled: true,
  downloadHandoffMode: 'ask',
  contextMenuEnabled: true,
  showProgressAfterHandoff: true,
  showBadgeStatus: true,
  excludedHosts: [...DEFAULT_EXTENSION_EXCLUDED_HOSTS],
  ignoredFileExtensions: [],
  capturedFileExtensions: [...DEFAULT_CAPTURED_FILE_EXTENSIONS],
  downloadCaptureDebugLogging: false,
};

export function createDefaultExtensionSettings(): ExtensionIntegrationSettings {
  return {
    ...defaultExtensionSettings,
    excludedHosts: [...defaultExtensionSettings.excludedHosts],
    ignoredFileExtensions: [...defaultExtensionSettings.ignoredFileExtensions],
    capturedFileExtensions: [...defaultExtensionSettings.capturedFileExtensions],
  };
}

export function normalizeExtensionSettings(
  settings?: Partial<ExtensionIntegrationSettings>,
): ExtensionIntegrationSettings {
  const defaults = createDefaultExtensionSettings();
  return {
    ...defaults,
    ...settings,
    excludedHosts: normalizeHosts(settings?.excludedHosts ?? defaults.excludedHosts),
    ignoredFileExtensions: normalizeFileExtensions(
      settings?.ignoredFileExtensions ?? defaults.ignoredFileExtensions,
    ),
    capturedFileExtensions: normalizeFileExtensions(
      settings?.capturedFileExtensions ?? defaults.capturedFileExtensions,
    ),
  };
}

function normalizeHosts(hosts: string[]): string[] {
  return Array.from(new Set(hosts.map((host) => normalizeExcludedHostPattern(host)).filter(Boolean)));
}

function normalizeFileExtensions(values: string[]): string[] {
  const extensions = new Set<string>();
  for (const value of values) {
    for (const candidate of value.split(/[,\s]+/)) {
      let extension = candidate.trim().replace(/^\.+/, '').toLowerCase();
      if (extension === '7zip') extension = '7z';
      if (!extension || extension.includes('/') || extension.includes('\\')) continue;
      extensions.add(extension);
    }
  }
  return [...extensions];
}
