/**
 * The `device_name` a sign-in sends (spec 4.1), such as "Firefox on Linux", so
 * the account's device list tells browsers apart. Order matters: Edge and
 * Chrome both say "Chrome", Chrome and Safari both say "Safari", and Android
 * says "Linux".
 */
export function deviceName(userAgent: string): string {
  const browser = [
    ["Edg/", "Edge"],
    ["Firefox/", "Firefox"],
    ["Chrome/", "Chrome"],
    ["Safari/", "Safari"],
  ].find(([token]) => userAgent.includes(token!))?.[1];
  const system = [
    ["Android", "Android"],
    ["iPhone", "iOS"],
    ["iPad", "iOS"],
    ["Windows", "Windows"],
    ["Mac OS X", "macOS"],
    ["Linux", "Linux"],
  ].find(([token]) => userAgent.includes(token!))?.[1];
  if (!browser) return "Web browser";
  return system ? `${browser} on ${system}` : browser;
}
