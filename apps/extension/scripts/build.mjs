import { build } from 'esbuild';
import { access, cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const appRoot = path.resolve(__dirname, '..');
const distRoot = path.join(appRoot, 'dist');
const pkg = JSON.parse(readFileSync(path.join(appRoot, 'package.json'), 'utf8'));
const version = pkg.version || '0.1.0';

const extensionIcons = {
  16: 'icons/icon-16.png',
  32: 'icons/icon-32.png',
  48: 'icons/icon-48.png',
  128: 'icons/icon-128.png',
};

/** Locked Firefox id — must match native-host registration `allowed_extensions`. */
export const FIREFOX_EXTENSION_ID = 'rusticdl@local';

const targets = [
  {
    name: 'chromium',
    format: 'esm',
    scriptType: 'module',
    manifest: {
      manifest_version: 3,
      name: 'RusticDL',
      version,
      description: 'Send downloads to RusticDL.',
      icons: extensionIcons,
      permissions: [
        'alarms',
        'contextMenus',
        'cookies',
        'downloads',
        'nativeMessaging',
        'storage',
        'webRequest',
      ],
      host_permissions: ['<all_urls>'],
      background: {
        service_worker: 'background.js',
        type: 'module',
      },
      action: {
        default_title: 'RusticDL',
        default_popup: 'popup.html',
        default_icon: extensionIcons,
      },
      options_ui: {
        page: 'options.html',
        open_in_tab: true,
      },
    },
  },
  {
    name: 'firefox',
    // Classic scripts: Firefox MV2 background pages do not use type:module by default.
    format: 'iife',
    scriptType: 'classic',
    manifest: {
      manifest_version: 2,
      name: 'RusticDL',
      version,
      version_name: version,
      description: 'Send downloads to RusticDL.',
      icons: extensionIcons,
      permissions: [
        'alarms',
        'contextMenus',
        'cookies',
        'downloads',
        'nativeMessaging',
        'storage',
        'webRequest',
        'webRequestBlocking',
        'webRequestFilterResponse',
        '<all_urls>',
      ],
      background: {
        scripts: ['background.js'],
        // Persistent so blocking webRequest + ghost erase still run if Firefox
        // would otherwise suspend the event page mid-handoff.
        persistent: true,
      },
      browser_action: {
        default_title: 'RusticDL',
        default_popup: 'popup.html',
        default_icon: extensionIcons,
      },
      options_ui: {
        page: 'options.html',
        open_in_tab: true,
      },
      // Required for current Firefox installs (built-in data consent).
      browser_specific_settings: {
        gecko: {
          id: FIREFOX_EXTENSION_ID,
          // 140+ supports data_collection_permissions install UX.
          strict_min_version: '140.0',
          data_collection_permissions: {
            // Local-only handoff still touches browsing URLs, page metadata,
            // and download/response content used to classify captures.
            required: ['browsingActivity', 'websiteActivity', 'websiteContent'],
          },
        },
      },
    },
  },
];

await rm(distRoot, { recursive: true, force: true });

for (const target of targets) {
  const outDir = path.join(distRoot, target.name);
  await mkdir(outDir, { recursive: true });
  await mkdir(path.join(outDir, 'icons'), { recursive: true });

  await build({
    entryPoints: {
      background: path.join(appRoot, 'src/background/index.ts'),
      popup: path.join(appRoot, 'src/popup/index.ts'),
      options: path.join(appRoot, 'src/options/index.ts'),
    },
    bundle: true,
    outdir: outDir,
    format: target.format,
    platform: 'browser',
    target: target.name === 'firefox' ? ['firefox140'] : ['chrome110'],
    sourcemap: true,
    logLevel: 'info',
  });

  await writeFile(path.join(outDir, 'manifest.json'), `${JSON.stringify(target.manifest, null, 2)}\n`);

  await writeHtml(
    path.join(appRoot, 'src/popup/index.html'),
    path.join(outDir, 'popup.html'),
    target.scriptType,
    'popup.js',
  );
  await writeHtml(
    path.join(appRoot, 'src/options/index.html'),
    path.join(outDir, 'options.html'),
    target.scriptType,
    'options.js',
  );

  await cp(path.join(appRoot, 'src/shared/theme.css'), path.join(outDir, 'theme.css'));
  if (await exists(path.join(appRoot, 'src/shared/appearance-preload.js'))) {
    await cp(
      path.join(appRoot, 'src/shared/appearance-preload.js'),
      path.join(outDir, 'appearance-preload.js'),
    );
  }
  for (const file of Object.values(extensionIcons)) {
    await cp(path.join(appRoot, 'src', file), path.join(outDir, file));
  }

  console.log(`Built ${target.name} → ${outDir}`);
  console.log(`  manifest_version=${target.manifest.manifest_version} format=${target.format}`);
}

async function writeHtml(srcPath, destPath, scriptType, scriptName) {
  let html = await readFile(srcPath, 'utf8');
  // Only rewrite the page entry script (popup.js / options.js). Leave helpers
  // such as appearance-preload.js untouched.
  const entrySrcPattern = /src="\.\/(?:popup|options)\.js"/g;
  if (scriptType === 'classic') {
    // Firefox MV2 popup/options use classic scripts when bundle is IIFE.
    html = html.replace(
      /<script\s+type="module"\s+src="\.\/(popup|options)\.js"><\/script>/g,
      `<script src="./$1.js"></script>`,
    );
    html = html.replace(entrySrcPattern, `src="./${scriptName}"`);
  } else {
    // Chromium keeps type=module for ESM bundles.
    html = html.replace(entrySrcPattern, `src="./${scriptName}"`);
    html = html.replace(
      new RegExp(`<script\\s+src="\\./${scriptName.replace('.', '\\.')}"></script>`, 'g'),
      `<script type="module" src="./${scriptName}"></script>`,
    );
  }
  await writeFile(destPath, html);
}

async function exists(filePath) {
  try {
    await access(filePath);
    return true;
  } catch {
    return false;
  }
}
