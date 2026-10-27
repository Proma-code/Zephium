import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';
import { effectiveConfig, merge, root, validate } from './windows-trial.mjs';

test('platform overlays preserve a separate package and all legal resources', () => {
  validate();
});

test('the config merge removes upstream updater keys rather than shallow-merging them', () => {
  assert.deepEqual(merge({ plugins: { updater: { endpoints: ['upstream'] } } }, { plugins: { updater: null } }), { plugins: {} });
});

test('product, executable, updater and browser association regressions fail closed', () => {
  for (const patch of [
    { identifier: 'app.zephium' },
    { productName: 'Zephium' },
    { mainBinaryName: 'zephium-desktop' },
    { plugins: { updater: { endpoints: ['https://example.invalid/latest.json'] } } },
    { bundle: { createUpdaterArtifacts: true } },
    { bundle: { fileAssociations: [{ ext: ['html'] }] } },
    { bundle: { windows: { nsis: { installerHooks: 'windows/installer-hooks.nsh' } } } },
  ]) assert.throws(() => validate(merge(effectiveConfig(), patch)));
});

test('installer hook cannot clean up product data or browser registration', () => {
  const hook = readFileSync(resolve(root, 'desktop/windows/subscription-trial-hooks.nsh'), 'utf8');
  assert.match(hook, /StrCmp \$INSTDIR "\$LOCALAPPDATA\\Zephium Subscription Trial"/);
  assert.match(hook, /IfFileExists "\$INSTDIR\\zephium-desktop\.exe"/);
  assert.doesNotMatch(hook, /^\s*(Delete|DeleteRegKey|RMDir|Exec|ExecWait)\b/im);
  assert.deepEqual(hook.split(/\r?\n/).map((line) => line.trim()).filter((line) => /^DeleteRegValue\b/.test(line)), [
    'DeleteRegValue HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Run" "Zephium Subscription Trial"',
    'DeleteRegValue HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run" "Zephium Subscription Trial"',
  ]);
  assert.match(hook, /!macro NSIS_HOOK_POSTUNINSTALL\s+\$\{If\} \$UpdateMode <> 1\s+DeleteRegValue[^\n]+\n\s+DeleteRegValue[^\n]+\n\s+\$\{EndIf\}\s+!macroend/);
  assert.doesNotMatch(hook, /^\s*!include/im);
});

test('all native startup registry operations use the one trial-aware name without a product fallback', () => {
  const startup = readFileSync(resolve(root, 'desktop/src/login_item.rs'), 'utf8');
  assert.match(startup, /const NAME: &str = super::windows_startup_identity::login_item_name\(cfg!\(\s*feature = "windows-subscription-trial"\s*\)\);/);
  assert.equal([...startup.matchAll(/let name = wide\(NAME\);/g)].length, 3, 'read, write and delete must use the selected name');
  assert.doesNotMatch(startup, /(?:wide|read|write|delete)\("Zephium"\)/);
});

test('native release build requires both isolated feature and config', () => {
  const manifest = readFileSync(resolve(root, 'desktop/Cargo.toml'), 'utf8');
  assert.match(manifest, /windows-subscription-trial = .*zephium-credentials\/windows-subscription-trial/);
  assert.doesNotMatch(manifest, /zephium-(chatgpt|gateway|opencode-go)/);
  const build = readFileSync(resolve(root, 'desktop/build.rs'), 'utf8');
  assert.match(build, /windows_trial_config::validate\(&config, "windows", windows_trial\)/);
  const updates = readFileSync(resolve(root, 'desktop/src/updates.rs'), 'utf8');
  assert.match(updates, /const SUPPORTED:[\s\S]*?!cfg!\(feature = "windows-subscription-trial"\)/);
  const app = readFileSync(resolve(root, 'desktop/src/lib.rs'), 'utf8');
  assert.match(app, /#\[cfg\(not\(feature = "windows-subscription-trial"\)\)\]\s*let builder = builder\.plugin\(tauri_plugin_updater/);
  const defaults = readFileSync(resolve(root, 'desktop/src/default_browser.rs'), 'utf8');
  assert.match(defaults, /can_request: !cfg!\(feature = "windows-subscription-trial"\)/);
  assert.match(defaults, /if cfg!\(feature = "windows-subscription-trial"\) \{\s*return Some\(read_status\(\)\.await\);/);
});

test('trial CI is branch-only, read-only and never publishes a release', () => {
  const workflow = readFileSync(resolve(root, '.github/workflows/windows-subscription-trial.yml'), 'utf8');
  assert.match(workflow, /on:\s*push:\s*branches: \[windows\/subscription-accounts\]/);
  assert.match(workflow, /permissions:\s*contents: read/);
  assert.match(workflow, /if: github\.repository == 'Proma-code\/Zephium'/);
  assert.doesNotMatch(workflow, /^\s*(pull_request|pull_request_target|workflow_dispatch|schedule|tags|environment):/m);
  assert.doesNotMatch(workflow, /\$\{\{\s*secrets\.|contents:\s*write|gh release|git push/);
  for (const match of workflow.matchAll(/uses:\s*(\S+)/g)) {
    assert.match(match[1], /@[a-f0-9]{40}$/);
  }
});
