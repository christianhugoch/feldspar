// A one-shot message handed from one screen to the next.
//
// The applications screen already has the banner that shows a build's log or its
// diagnostics. Creating a React application produces the *same kind* of news —
// "the project was scaffolded", or why it was not (§2.3) — but it happens on the
// form, which navigates away immediately afterwards. Rather than grow a second
// banner on the form, the form leaves the message here and the list picks it up.
//
// Deliberately a module-level variable and not React state or storage: it is read
// exactly once, by the next screen to mount, and a message that outlived a reload
// would be a message about something the admin can no longer see.

/** A message to show on the next screen: an outcome, a heading and its detail. */
export type Notice = {
  /** Whether this reports success (green) or a failure (red). */
  ok: boolean;
  /** The banner heading, e.g. `Application created — Todo`. */
  title: string;
  /** The detail: a log, a summary line, or an error message. */
  text: string;
};

let pending: Notice | null = null;

/** Leave a message for the next screen. */
export function setNotice(notice: Notice): void {
  pending = notice;
}

/** Take the pending message, if any. Reading it clears it. */
export function takeNotice(): Notice | null {
  const notice = pending;
  pending = null;
  return notice;
}
