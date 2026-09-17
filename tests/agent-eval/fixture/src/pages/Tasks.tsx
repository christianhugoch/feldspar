import { useEffect, useState } from "react";

import { createTask, listTasks, type Task } from "../feldspar/client";
import { formatDue } from "../lib/due";

export function TasksPage() {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [title, setTitle] = useState("");

  useEffect(() => {
    listTasks().then(setTasks);
  }, []);

  async function add(event: React.FormEvent) {
    event.preventDefault();
    if (title.trim() === "") {
      return;
    }
    const created = await createTask({ title, done: false, due: null });
    setTasks((current) => [...current, created]);
    setTitle("");
  }

  return (
    <main>
      <h1>Tasks</h1>
      <form onSubmit={add}>
        <input
          aria-label="Title"
          value={title}
          onChange={(event) => setTitle(event.target.value)}
        />
        <button type="submit">Add</button>
      </form>
      <ul className="task-list">
        {tasks.map((task) => (
          <li className="task" key={task.id}>
            <input type="checkbox" checked={task.done} readOnly />
            <span>{task.title}</span>
            <span className="task-due">{formatDue(task.due)}</span>
          </li>
        ))}
      </ul>
    </main>
  );
}
