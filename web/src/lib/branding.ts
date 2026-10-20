import manifest from '../../../overlays/enact/brand.json';

// Presentation only. Protocols, config paths, API keys, and upstream links retain
// their original identities so upstream upgrades and existing installs work.
export const brand = manifest;

export function brandDisplayText(text: string): string {
  return text.replace(/ZeroClaw Agent|ZeroClaw/g, brand.name);
}

export function brandLogo(theme: 'light' | 'dark' | 'oled'): string {
  return theme === 'light' ? brand.logo : brand.logoDark;
}
