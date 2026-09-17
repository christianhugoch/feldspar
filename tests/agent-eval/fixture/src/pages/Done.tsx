import { useEffect, useState } from "react";

import { listTasks, type Task } from "../feldspar/client";

export function DonePage() {
  const [tasks, setTasks] = useState<Task[]>([]);

  useEffect(() => {
    listTasks().then(setTasks);
  }, []);

  const done = tasks.filter((task) => task.done);

  return (
    <main>
      <h1>Done</h1>
      <ul className="task-list">
        {done.map((task) => (
          <li className="task" key={task.id}>
            <span>{task.title}</span>
          </li>
        ))}
      </ul>
    </main>
  );
}
