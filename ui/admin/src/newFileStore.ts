// "Create a new local file store" on a new application's store picker.
//
// An admin creating their first React application has no store to put its code
// in, and used to have to leave the form, define one, and come back. The picker
// now ends with a choice that asks the server to make one: it names the store
// after the subdomain (appending 1, 2, … when that is taken) and puts it where
// the file-store form's "Suggest a directory" would (`createApplication`).
//
// A module rather than inline in the screen for the reason `staticDirs.ts` is
// one: which pickers get the choice, and what the admin is told afterwards, are
// decisions testable without a browser (`newFileStore.test.ts`).

/** What a store setting holds to mean "create a local store for me"
 * (`sc_catalog::NEW_LOCAL_FILE_STORE`).
 *
 * Duplicated rather than imported because it is a *protocol* constant — it
 * travels in the JSON — like `SECRET_SENTINEL` in `settings.tsx`. The server
 * replaces it with the new store's name before anything is saved, and reserves
 * it as a store name so it can never mean an existing one. */
export const NEW_LOCAL_FILE_STORE = "__new_local_file_store__";

/** An option a screen adds after a setting's own, with a label of its own. */
export type ExtraOption = { value: string; label: string };

/** The extra options for a framework's settings: the new-store choice on every
 * setting that names a file store, when the application is being **created**.
 *
 * Only on create, because that is the only request that acts on it — moving an
 * existing application's code to a new, empty store is not something a
 * drop-down should do as a side effect of Save. */
export function newStoreOptions(
  fileStoreSettings: readonly string[],
  creating: boolean,
  label: string,
): Record<string, ExtraOption[]> {
  if (!creating) return {};
  return Object.fromEntries(
    fileStoreSettings.map((name) => [name, [{ value: NEW_LOCAL_FILE_STORE, label }]]),
  );
}

/** The sentence the applications list's banner adds for the stores created
 * with the application, or null when there were none. */
export function createdStoresText(created: readonly string[] | null | undefined): string | null {
  if (!created || created.length === 0) return null;
  const names = created.map((n) => `“${n}”`).join(", ");
  return created.length === 1
    ? `A new local file store, ${names}, was created for its code.`
    : `New local file stores ${names} were created for its code.`;
}
