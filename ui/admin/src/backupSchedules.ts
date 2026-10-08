/**
 * The Automated backups card's model: the form a schedule is edited in, what
 * it checks before it is sent, and what a schedule's line says about it.
 *
 * The server checks everything again (and checks what only it can — that the
 * directory can be written, that no other schedule writes there); these checks
 * are here so the obvious mistakes are reported beside the field rather than
 * after a round trip.
 */

import type { ListBackupSchedulesResponse } from "./client";

export type BackupSchedule = ListBackupSchedulesResponse[number];

export type Frequency = "daily" | "weekly";

export const FREQUENCIES: { value: Frequency; label: string }[] = [
  { value: "daily", label: "Daily" },
  { value: "weekly", label: "Weekly" },
];

/** The most days a backup can be kept — the server's `MAX_RETENTION_DAYS`. */
export const MAX_RETENTION_DAYS = 3650;

/** The schedule being added or edited, as the form holds it: `id` is null for
 * a new one, and the retention is the text in its box. */
export type ScheduleForm = {
  id: string | null;
  destination: string;
  frequency: Frequency;
  retention: string;
};

/** What the Add button opens on. */
export function newScheduleForm(): ScheduleForm {
  return { id: null, destination: "", frequency: "daily", retention: "30" };
}

/** What the Edit button opens on. */
export function editScheduleForm(schedule: BackupSchedule): ScheduleForm {
  return {
    id: schedule.id,
    destination: schedule.destination,
    frequency: schedule.frequency === "weekly" ? "weekly" : "daily",
    retention: String(schedule.retention_days),
  };
}

/** Why the form cannot be sent, by field; empty when it can. */
export function scheduleFormErrors(
  form: ScheduleForm,
): Partial<Record<"destination" | "retention", string>> {
  const errors: Partial<Record<"destination" | "retention", string>> = {};
  const destination = form.destination.trim();
  if (destination === "") errors.destination = "Enter a directory on the server.";
  else if (!destination.startsWith("/"))
    errors.destination = "Enter an absolute path, starting with /.";
  else if (destination.split("/").includes(".."))
    errors.destination = "Enter the path without '..'.";
  const retention = form.retention.trim();
  const days = Number(retention);
  if (!/^\d+$/.test(retention) || days < 1 || days > MAX_RETENTION_DAYS)
    errors.retention = `Enter a whole number of days from 1 to ${MAX_RETENTION_DAYS}.`;
  return errors;
}

/** The request body the form becomes. Only call it on a form with no errors. */
export function scheduleBody(form: ScheduleForm) {
  return {
    destination: form.destination.trim(),
    frequency: form.frequency,
    retention_days: Number(form.retention.trim()),
  };
}

/** "Daily" or "Weekly". */
export function frequencyLabel(frequency: string): string {
  return FREQUENCIES.find((f) => f.value === frequency)?.label ?? frequency;
}

/** What a schedule last did, in one line, and whether it is a problem. */
export function scheduleStatus(
  schedule: BackupSchedule,
  formatTime: (iso: string) => string = (iso) => new Date(iso).toLocaleString(),
): { text: string; failed: boolean } {
  if (schedule.last_error && schedule.last_attempt_at)
    return {
      text: `Failed ${formatTime(schedule.last_attempt_at)}: ${schedule.last_error}`,
      failed: true,
    };
  if (schedule.last_success_at)
    return { text: `Last backup ${formatTime(schedule.last_success_at)}`, failed: false };
  return { text: "Not run yet — the first backup is taken within a minute.", failed: false };
}
