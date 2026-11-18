import type { PopupRequest } from '../shared/messages';

const POPUP_REQUEST_TYPES: ReadonlySet<string> = new Set<PopupRequest['type']>([
  'popup_ping',
  'popup_get_state',
  'popup_open_app',
  'popup_open_options',
  'extension_settings_update',
  'appearance_settings_get',
]);

/**
 * Only this extension's own pages (popup / options) may drive the
 * background. There are no content scripts, so anything else — another
 * extension, or a page if `externally_connectable` is ever added — is
 * dropped before it can change capture settings or open the app.
 */
export function isTrustedPopupSender(
  sender: { id?: string; url?: string; tab?: unknown } | undefined,
  extensionId: string,
  extensionBaseUrl: string,
): boolean {
  if (!sender || sender.id !== extensionId) return false;
  if (!sender.url) return false;
  return sender.url.startsWith(extensionBaseUrl);
}

export function isPopupRequest(message: unknown): message is PopupRequest {
  if (!message || typeof message !== 'object') return false;
  const type = (message as { type?: unknown }).type;
  if (typeof type !== 'string' || !POPUP_REQUEST_TYPES.has(type)) return false;
  if (type === 'extension_settings_update') {
    const settings = (message as { settings?: unknown }).settings;
    return Boolean(settings && typeof settings === 'object');
  }
  return true;
}
