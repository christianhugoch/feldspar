// Telling the admin something, the way the builder does: v1's `notifyAlert`,
// which the host document's `saltcorn-common.js` defines (`globals.ts`).

export type Note = { type: "info" | "success" | "warning" | "danger"; text: string };

export function notify(note: Note): void {
  const notifyAlert = (window as unknown as { notifyAlert?: (note: Note) => void }).notifyAlert;
  if (notifyAlert) notifyAlert(note);
  else console.warn(note.text);
}
