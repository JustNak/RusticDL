export const HTML_DOWNLOAD_HINT_MESSAGE_TYPE = 'rusticdl_html_download_hint' as const;

export type HtmlDownloadHintMessage = {
  type: typeof HTML_DOWNLOAD_HINT_MESSAGE_TYPE;
  url: string;
  filename: string;
};

export function isHtmlDownloadHintMessage(value: unknown): value is HtmlDownloadHintMessage {
  if (!value || typeof value !== 'object') return false;
  const record = value as { type?: unknown; url?: unknown; filename?: unknown };
  return (
    record.type === HTML_DOWNLOAD_HINT_MESSAGE_TYPE
    && typeof record.url === 'string'
    && record.url.length > 0
    && typeof record.filename === 'string'
    && record.filename.length > 0
  );
}
