// `utils.ts` computes `pluginsFolderRoot` from `os.homedir()` at module load —
// where v1 installs plugins, a path no view reads. It is the only `os` call in
// the vendored files, and in the module worker it is not a free one: the view
// runtime has an empty permission set (TODO §3), so asking for the home
// directory would fail the whole bundle's evaluation. There is no home here.
export const homedir = (): string => "";

export default { homedir };
