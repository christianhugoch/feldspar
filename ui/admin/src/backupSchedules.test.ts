/**
 * The Automated backups card's model: the form's checks, the body it sends,
 * and the status line each schedule shows.
 */

import { describe, expect, it } from "vitest";

import {
  editScheduleForm,
  newScheduleForm,
  scheduleBody,
  scheduleFormErrors,
  scheduleStatus,
  type BackupSchedule,
} from "./backupSchedules";

const schedule: BackupSchedule = {
  id: "6f9619ff-8b86-d011-b42d-00c04fc964ff",
  destination: "/srv/backups",
  frequency: "weekly",
  retention_days: 30,
  last_attempt_at: null,
  last_success_at: null,
  last_error: null,
  last_file: null,
};

describe("the schedule form", () => {
  it("opens empty for a new schedule and on the schedule for an edit", () => {
    expect(newScheduleForm()).toMatchObject({ id: null, destination: "", frequency: "daily" });
    expect(editScheduleForm(schedule)).toEqual({
      id: schedule.id,
      destination: "/srv/backups",
      frequency: "weekly",
      retention: "30",
    });
  });

  it("wants an absolute destination", () => {
    const form = { ...newScheduleForm(), retention: "7" };
    expect(scheduleFormErrors({ ...form, destination: "" }).destination).toBeDefined();
    expect(scheduleFormErrors({ ...form, destination: "backups" }).destination).toBeDefined();
    expect(scheduleFormErrors({ ...form, destination: "/srv/../etc" }).destination).toBeDefined();
    expect(scheduleFormErrors({ ...form, destination: "/srv/backups" })).toEqual({});
  });

  it("wants a whole number of days of at least one", () => {
    const form = { ...newScheduleForm(), destination: "/srv/b" };
    for (const bad of ["", "0", "-3", "1.5", "seven", "99999"])
      expect(scheduleFormErrors({ ...form, retention: bad }).retention).toBeDefined();
    expect(scheduleFormErrors({ ...form, retention: " 14 " })).toEqual({});
  });

  it("sends trimmed values and a number of days", () => {
    expect(
      scheduleBody({ id: null, destination: " /srv/b ", frequency: "weekly", retention: "14" }),
    ).toEqual({ destination: "/srv/b", frequency: "weekly", retention_days: 14 });
  });
});

describe("a schedule's status line", () => {
  const time = (iso: string) => iso.slice(0, 10);

  it("says when it has not run yet", () => {
    expect(scheduleStatus(schedule, time)).toMatchObject({ failed: false });
    expect(scheduleStatus(schedule, time).text).toMatch(/Not run yet/);
  });

  it("says when it last succeeded", () => {
    const ran = { ...schedule, last_success_at: "2026-10-01T02:00:00Z" };
    expect(scheduleStatus(ran, time)).toEqual({ text: "Last backup 2026-10-01", failed: false });
  });

  it("says why it last failed, ahead of an earlier success", () => {
    const failed = {
      ...schedule,
      last_success_at: "2026-10-01T02:00:00Z",
      last_attempt_at: "2026-10-02T02:00:00Z",
      last_error: "disk full",
    };
    expect(scheduleStatus(failed, time)).toEqual({
      text: "Failed 2026-10-02: disk full",
      failed: true,
    });
  });
});
