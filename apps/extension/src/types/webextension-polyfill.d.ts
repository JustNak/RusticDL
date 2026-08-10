/**
 * Minimal typing bridge: reuse the ambient `browser` API from
 * `@types/firefox-webext-browser` for the polyfill default export.
 */
declare module "webextension-polyfill" {
  const browser: typeof globalThis.browser;
  export default browser;
}
