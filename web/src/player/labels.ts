/** "1.3×", the way the speed steps are written. */
export function speedLabel(speed: number): string {
  return `${Number(speed.toFixed(2))}×`;
}

/** "15 s", or whole minutes. */
export function skipLabel(ms: number): string {
  return ms >= 60_000 && ms % 60_000 === 0 ? `${ms / 60_000} min` : `${ms / 1_000} s`;
}

/** What kind of device another Connect device is, under its name in the output picker. */
export function deviceKindLabel(kind: "web" | "desktop" | "android"): string {
  switch (kind) {
    case "web":
      return "Browser";
    case "desktop":
      return "Desktop app";
    case "android":
      return "Android";
  }
}
