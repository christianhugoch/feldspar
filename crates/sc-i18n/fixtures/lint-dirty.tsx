// A fixture for `feldspar i18n lint`: one of each thing the lint reports, and
// nothing else. Not compiled — it is read as bytes by the test beside it.
import { useT, T } from "./i18n";

export function TaskRow({ name }: { name: string }) {
  const t = useT();
  return (
    <div className="task-row" id="task-row">
      <span>Overdue since yesterday</span>
      <input placeholder="Search tasks" name="q" type="search" />
      <button title="Remove this task" onClick={() => remove(name)}>
        {t("Delete")}
      </button>
      <a aria-label="Open in a new tab" href="/tasks">
        {"→"}
      </a>
      <label label="Due date">{t("Due")}</label>
    </div>
  );
}
