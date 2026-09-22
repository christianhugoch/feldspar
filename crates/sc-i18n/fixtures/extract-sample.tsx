// Every shape the extractor has to read, and the two it has to refuse.
import { useT, T, tc } from "./i18n";

export function Screen({ count, title }: { count: number; title: string }) {
  const t = useT();
  const heading = t(`Add a task`);
  const plural = t("{count} rows", { count });
  const noun = tc("verb", "Order");
  const quoted = t("Delete \"{name}\"?", { name: title });
  // Neither of these is a literal, and both are errors rather than skips.
  const computed = t(title);
  const interpolated = t(`Hello ${title}`);
  return (
    <div>
      <T text="Add a task" />
      <T text="Order" context="noun" />
      {heading}
      {plural}
      {noun}
      {quoted}
      {computed}
      {interpolated}
    </div>
  );
}
