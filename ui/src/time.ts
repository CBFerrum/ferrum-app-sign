// Time as the screens say it. Pure, so the words are tested without a clock.

export const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

export interface Left {
  text: string;
  /** `soon`: worth a nudge (under three days); `late`: over. */
  level: "ok" | "soon" | "late";
}

/** How long is left until a deadline: "5 days left", "3 hours left", "Expired". */
export function timeLeft(deadline: string | null | undefined, now = Date.now()): Left | null {
  const at = deadline ? new Date(deadline).getTime() : NaN;
  if (Number.isNaN(at)) return null;
  const minutes = Math.floor((at - now) / 60_000);
  if (minutes <= 0) return { text: "Expired", level: "late" };
  if (minutes < 60) return { text: "Under an hour left", level: "soon" };
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return { text: `${plural(hours, "hour")} left`, level: "soon" };
  const days = Math.floor(hours / 24);
  return { text: `${plural(days, "day")} left`, level: days < 3 ? "soon" : "ok" };
}

/** "3 Nov, 2:15 pm": a moment in the viewer's time. */
export function moment(iso: string | null | undefined): string {
  const t = iso ? new Date(iso) : null;
  if (!t || Number.isNaN(t.getTime())) return "";
  return t.toLocaleString(undefined, { day: "numeric", month: "short", hour: "numeric", minute: "2-digit" });
}

/** A deadline with its year, for the signer's page: "14 Nov 2026, 5:00 pm". */
export function deadline(iso: string | null | undefined): string {
  const t = iso ? new Date(iso) : null;
  if (!t || Number.isNaN(t.getTime())) return "";
  return t.toLocaleString(undefined, { day: "numeric", month: "short", year: "numeric", hour: "numeric", minute: "2-digit" });
}
