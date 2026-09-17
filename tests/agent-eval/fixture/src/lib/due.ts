// How a task's due date is shown in the list.

/** Whole days from `from` to `date`, as the list counts them. */
export function daysUntil(date: Date, from: Date): number {
  const ms = date.getTime() - from.getTime();
  return Math.floor(ms / (1000 * 60 * 60 * 24));
}

/**
 * A task's due date in words: "today", "tomorrow", "in 3 days", or "2 days
 * overdue".
 */
export function formatDue(due: string | null, now: Date = new Date()): string {
  if (due === null) {
    return "no due date";
  }
  const days = daysUntil(new Date(due), now);
  if (days < 0) {
    return `${-days} days overdue`;
  }
  if (days === 0) {
    return "today";
  }
  return `in ${days} days`;
}
