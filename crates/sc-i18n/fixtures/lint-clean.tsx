// The same screen with every string wrapped: the lint must say nothing about
// this file, including about the attributes a person never reads.
import { useT, T } from "./i18n";

export function TaskRow({ name }: { name: string }) {
  const t = useT();
  return (
    <div className="task-row" id="task-row" data-kind="task">
      <span>{t("Overdue since yesterday")}</span>
      <input placeholder={t("Search tasks")} name="q" type="search" />
      <button title={t("Remove this task")} onClick={() => remove(name)}>
        <T text="Delete" />
      </button>
      <a aria-label={t("Open in a new tab")} href="/tasks">
        {"→"}
      </a>
      <label>{t("Due")}</label>
    </div>
  );
}
