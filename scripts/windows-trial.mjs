#!/usr/bin/env node
// Only an explicit Windows build can produce the isolated experimental package.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const trialId = 'app.zephium.subscription-trial';
export const trialName = 'Zephium Subscription Trial';
export const trialBinary = 'zephium-subscription-trial';
export const configPath = 'desktop/tauri.windows-subscription-trial.conf.json';
const json = (path) => JSON.parse(readFileSync(resolve(root, path), 'utf8'));

// RFC 7396, the same merge-patch order used by Tauri's build script.
export function merge(base, patch) {
  if (patch === null || typeof patch !== 'object' || Array.isArray(patch)) return structuredClone(patch);
  const result = base && typeof base === 'object' && !Array.isArray(base) ? structuredClone(base) : {};
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete result[key];
    else result[key] = merge(result[key], value);
  }
  return result;
}

export function effectiveConfig() {
  return merge(merge(json('desktop/tauri.conf.json'), json('desktop/tauri.windows.conf.json')), json(configPath));
}

export function validate(config = effectiveConfig()) {
  assert.equal(config.identifier, trialId);
  assert.equal(config.productName, trialName);
  assert.equal(config.mainBinaryName, trialBinary);
  assert.equal(config.app.windows.length, 1);
  assert.equal(config.app.windows[0].title, trialName);
  assert.equal(config.app.windows[0].create, false);
  assert.equal(config.app.windows[0].label, 'main');
  assert.equal(config.build.devUrl, undefined);
  assert.equal(config.plugins.updater, undefined);
  assert.equal(config.plugins['deep-link'], undefined);
  assert.equal(config.bundle.createUpdaterArtifacts, false);
  assert.deepEqual(config.bundle.targets, ['nsis']);
  assert.deepEqual(config.bundle.fileAssociations, []);
  assert.equal(config.bundle.windows.webviewInstallMode.type, 'skip');
  assert.equal(config.bundle.windows.nsis.installMode, 'currentUser');
  assert.equal(config.bundle.windows.nsis.startMenuFolder, trialName);
  assert.equal(config.bundle.windows.nsis.installerHooks, 'windows/subscription-trial-hooks.nsh');
  for (const [source, destination] of Object.entries(json('desktop/tauri.conf.json').bundle.resources)) {
    assert.equal(config.bundle.resources[source], destination);
  }
  assert.equal(config.bundle.resources['../target/THIRD_PARTY_NOTICES.txt'], 'licenses/THIRD_PARTY_NOTICES.txt');
  return config;
}

function run(command, args, cwd = root) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const mode = process.argv[2] ?? '--check';
  assert.ok(['--check', '--build'].includes(mode) && process.argv.length <= 3, 'Use --check or --build, without arbitrary build overrides');
  validate();
  if (mode === '--build') {
    assert.equal(process.platform, 'win32', 'Build this package on Windows; host-side guards are not native qualification');
    assert.equal(process.arch, 'x64', 'The trial package currently targets Windows x64 only');
    for (const variable of ['TAURI_CONFIG', 'TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PATH']) {
      assert.equal(process.env[variable], undefined, `${variable} must not override this experimental build`);
    }
    run('python', ['scripts/release/third_party_notices.py', 'target/THIRD_PARTY_NOTICES.txt']);
    run('pnpm', ['exec', 'tauri', 'build', '--bundles', 'nsis', '--features', 'windows-subscription-trial', '--config', 'tauri.windows-subscription-trial.conf.json', '--', '--locked'], resolve(root, 'desktop'));
  } else {
    console.log('Windows trial configuration isolation passed (not native runtime verification).');
  }
}
