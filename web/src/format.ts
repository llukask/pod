// Wording for dates and durations, the same on every screen (wireframes D1, D2).

const DAY_MS = 86_400_000;
const weekday = new Intl.DateTimeFormat("en", { weekday: "short" });
const monthDay = new Intl.DateTimeFormat("en", { month: "short", day: "numeric" });
const monthDayYear = new Intl.DateTimeFormat("en", { month: "short", day: "numeric", year: "numeric" });

/** "Today", "Yesterday", a weekday within the last week, then "Oct 8", with the year once it differs. */
export function dateLabel(iso: string, now: Date = new Date()): string {
  const date = new Date(iso);
  const days = Math.round((startOfDay(now) - startOfDay(date)) / DAY_MS);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return weekday.format(date);
  return date.getFullYear() === now.getFullYear() ? monthDay.format(date) : monthDayYear.format(date);
}

function startOfDay(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/** "28 min", "1 h", "1 h 4 min", rounded to the nearest minute; a short clip still reads "1 min". */
export function durationLabel(ms: number): string {
  return minutesLabel(ms > 0 ? Math.max(1, Math.round(ms / 60_000)) : 0);
}

/** "21 min left", rounded up so an unfinished episode never reads "0 min left". */
export function leftLabel(durationMs: number, positionMs: number): string {
  return `${minutesLabel(Math.ceil(Math.max(durationMs - positionMs, 0) / 60_000))} left`;
}

function minutesLabel(minutes: number): string {
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours === 0) return `${rest} min`;
  return rest === 0 ? `${hours} h` : `${hours} h ${rest} min`;
}

/** "31:12", or "1:02:03" from an hour on. Truncates, like every player's elapsed time. */
export function clockLabel(ms: number): string {
  return clock(Math.floor(Math.max(ms, 0) / 1_000));
}

/** "−20:48": the time left, rounded up so it reads "−0:00" only at the end. */
export function remainingLabel(positionMs: number, durationMs: number): string {
  return `−${clock(Math.ceil(Math.max(durationMs - positionMs, 0) / 1_000))}`;
}

function clock(totalSeconds: number): string {
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = String(totalSeconds % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}` : `${minutes}:${seconds}`;
}
