/**
 * A failed build's output, as diagnostics (design §12.1, §16).
 *
 * The build runs on the server, and a failure is an ordinary API error whose
 * message carries the tail of what the tools said. That text is the only place the
 * type errors are, so it is parsed here into file/line/column/message and put in
 * the Problems panel — the same place the language server's diagnostics will land
 * in phase 4, and the reason those errors are wired to a panel rather than a toast.
 *
 * Three shapes are understood, because three tools speak:
 *
 * - `tsc`, on **stdout**: `src/App.tsx(12,5): error TS2322: Type 'x' is not …`
 * - esbuild and friends: `src/App.tsx:12:5: ERROR: Expected ";" but found "}"`
 * - rolldown/vite's boxed report, whose header names the position and whose
 *   message is the line above it:
 *
 *   ```text
 *   [builtin:vite-transform] 'export' modifier cannot be used here.
 *      ╭─[ src/main.ts:2:1 ]
 *   ```
 *
 * Anything else in the log is left alone: unparsed output is not lost, it goes to
 * the build's output channel whole.
 *
 * No VS Code in here either — the caller turns these into `vscode.Diagnostic`s —
 * so the parsing is testable on its own.
 */

/** One parsed diagnostic, in the store's own terms. */
export interface BuildDiagnostic {
  /** Store-relative path of the file, as the filesystem provider spells it. */
  readonly path: string;
  /** 1-based, as every one of these tools reports it. */
  readonly line: number;
  /** 1-based. */
  readonly column: number;
  readonly severity: "error" | "warning";
  readonly message: string;
}

/** `tsc --noEmit`, which is what type errors arrive as until phase 4. */
const TSC = /^(\S[^(]*)\((\d+),(\d+)\):\s*(error|warning)\s+([A-Za-z]+\d+):\s*(.*)$/;

/** esbuild, and the many tools that copy its one-line form. */
const FILE_LINE_COLUMN =
  /^\s*(?:\[[^\]]*\]\s*)?([^\s:][^\s:]*\.[A-Za-z0-9]+):(\d+):(\d+):\s*(?:(ERROR|WARNING|error|warning):\s*)?(\S.*)$/;

/** rolldown's boxed report: the frame header carries the position. */
const FRAME_HEADER = /[╭┌][─-]*\[\s*([^\s\]]+?):(\d+):(\d+)\s*\]/;

/** Terminal colour, which a build that thought it had a TTY leaves behind. */
const ANSI = /\u001b\[[0-9;?]*[A-Za-z]/g;

/** Box-drawing and rule characters: a line made of these carries no message. */
const FRAME_LINE = /^[\s╭╰│─┬┴┼┤├╯╮>0-9|:]*$/;

/**
 * Where the build ran, as the error message reports it.
 *
 * `run_build` fails with "build command `npm run build` failed in <dir> with …",
 * and `<dir>` is the absolute path of the source directory on the server. It is
 * what lets an absolute path in a diagnostic be recognised as a file in this
 * store; without it, absolute paths are simply not mapped.
 */
export function buildDirectoryOf(log: string): string | null {
  const match = log.match(/failed in (.+?) with /);
  return match ? match[1] : null;
}

/**
 * The diagnostics a failed build's output names, as store paths.
 *
 * `sourcePath` is the application's source directory relative to the store root
 * (§13.3): the tools report paths relative to it, because that is where they ran.
 */
export function parseBuildDiagnostics(log: string, sourcePath: string): BuildDiagnostic[] {
  const buildDir = buildDirectoryOf(log);
  const found: BuildDiagnostic[] = [];
  const seen = new Set<string>();
  let lastMessage = "";

  const add = (
    file: string,
    line: string,
    column: string,
    severity: string | undefined,
    message: string,
  ) => {
    const path = storePathOf(file, sourcePath, buildDir);
    if (path === null || message.trim() === "") return;
    const diagnostic: BuildDiagnostic = {
      path,
      line: Math.max(1, Number(line)),
      column: Math.max(1, Number(column)),
      severity: severity?.toLowerCase() === "warning" ? "warning" : "error",
      message: message.trim(),
    };
    const key = `${diagnostic.path}:${diagnostic.line}:${diagnostic.column}:${diagnostic.message}`;
    if (seen.has(key)) return;
    seen.add(key);
    found.push(diagnostic);
  };

  for (const raw of log.split(/\r?\n/)) {
    const text = raw.replace(ANSI, "").trimEnd();

    const frame = text.match(FRAME_HEADER);
    if (frame) {
      add(frame[1], frame[2], frame[3], undefined, lastMessage);
      continue;
    }

    const tsc = text.match(TSC);
    if (tsc) {
      add(tsc[1], tsc[2], tsc[3], tsc[4], `${tsc[5]}: ${tsc[6]}`);
      lastMessage = "";
      continue;
    }

    const positioned = text.match(FILE_LINE_COLUMN);
    if (positioned && !positioned[1].includes("://")) {
      add(positioned[1], positioned[2], positioned[3], positioned[4], positioned[5]);
      lastMessage = "";
      continue;
    }

    // Not a diagnostic itself, but possibly the message a frame below it is
    // about. Box-drawing lines and blank lines are not, and must not overwrite
    // the message that is.
    if (text.trim() !== "" && !FRAME_LINE.test(text)) lastMessage = text.trim();
  }

  return found;
}

/**
 * A path a tool reported, as a path in this store — or `null` when it names
 * something outside it.
 *
 * Relative paths are relative to the source directory, which is where the build
 * command ran. An absolute path is one the *server's* filesystem has, so it is
 * only meaningful when it lies under the directory the build reported, and is
 * dropped otherwise: a diagnostic pointing at a file the admin cannot open is
 * worse than one that only appears in the output channel.
 */
export function storePathOf(
  file: string,
  sourcePath: string,
  buildDirectory: string | null,
): string | null {
  let relative = file.trim().replace(/^\.\//, "");
  if (relative.startsWith("/")) {
    if (buildDirectory === null) return null;
    const prefix = buildDirectory.replace(/\/+$/, "");
    if (relative === prefix) return sourcePath === "" ? "" : sourcePath;
    if (!relative.startsWith(`${prefix}/`)) return null;
    relative = relative.slice(prefix.length + 1);
  }
  if (relative === "" || relative.includes("..")) return null;
  return sourcePath === "" ? relative : `${sourcePath}/${relative}`;
}
