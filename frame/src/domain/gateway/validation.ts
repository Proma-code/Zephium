/** Matches the native local-gateway boundary; validation is UX, never network admission. */
export function gatewayBaseValid(base: string): boolean {
  if (base.length > 64) return false;
  const match = /^http:\/\/(?:127\.0\.0\.1|\[::1\]):([1-9][0-9]{0,4})\/v1\/?$/u.exec(base);
  return !!match && Number(match[1]) <= 65535;
}

export function gatewayLabelValid(label: string): boolean {
  return (
    label.length > 0 &&
    new TextEncoder().encode(label).length <= 64 &&
    label.trim() === label &&
    !/\p{Cc}/u.test(label)
  );
}

export function gatewayKeyValid(key: string): boolean {
  return /^[\x21-\x7e]{1,1024}$/u.test(key);
}
