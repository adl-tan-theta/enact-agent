import assert from 'node:assert/strict';
import test from 'node:test';
import { Window } from 'happy-dom';
import { brand, brandDisplayText, brandLogo } from './branding';

Object.assign(globalThis, { window: new Window() });
const { t, tLocale, setLocale, SUPPORTED_LOCALES } = await import('./i18n');

test('all locales brand user-visible product text while retaining translations', () => {
  for (const { code } of SUPPORTED_LOCALES) {
    assert.equal(tLocale('sidebar.brand', code), brand.name);
    assert.equal(tLocale('sidebar.logo_alt', code), brand.name);
    assert.equal(tLocale('agentchat.empty_title', code), brand.name);
    assert.ok(tLocale('doctor.empty_hint', code).includes(brand.name));
    assert.ok(!tLocale('doctor.empty_hint', code).includes('ZeroClaw'));
  }
  assert.ok(tLocale('doctor.empty_hint', 'nl').startsWith('Klik'));
  setLocale('en');
  assert.equal(t('sidebar.brand'), brand.name);
});

test('branding preserves technical identifiers and translation fallback contract', () => {
  assert.equal(tLocale('sops.trigger_path_placeholder', 'en'), '~/.zeroclaw/config.toml');
  assert.equal(tLocale('dashboard.ram.resident', 'en'), 'resident (zeroclaw)');
  assert.equal(tLocale('missing.translation', 'en'), 'missing.translation');
  assert.equal(brandDisplayText('zeroclaw.v1 zeroclaw_token'), 'zeroclaw.v1 zeroclaw_token');
});

test('canonical logo variants follow the resolved dashboard theme', () => {
  assert.equal(brandLogo('light'), 'enact-logo.svg');
  assert.equal(brandLogo('dark'), 'enact-logo-dark.svg');
  assert.equal(brandLogo('oled'), 'enact-logo-dark.svg');
});
