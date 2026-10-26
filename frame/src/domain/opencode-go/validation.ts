/** Mirrors the native boundary. The form trims a pasted key before this validation. */
export function goKeyValid(key: string): boolean {
  return /^[\x21-\x7e]{1,1024}$/u.test(key);
}
