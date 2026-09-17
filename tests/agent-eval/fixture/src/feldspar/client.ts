// The application's generated API client. In a scaffolded Feldspar project this
// file is rewritten on every build from the application's endpoints: read it to
// learn what data there is, and never edit it.

export interface Task {
  id: number;
  title: string;
  done: boolean;
  due: string | null;
}

export type NewTask = Omit<Task, "id">;

const BASE = "/api";

async function json<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE}${path}`, {
    headers: { "content-type": "application/json" },
    ...init,
  });
  if (!response.ok) {
    throw new Error(`${init?.method ?? "GET"} ${path} failed: ${response.status}`);
  }
  return (await response.json()) as T;
}

export function listTasks(): Promise<Task[]> {
  return json<Task[]>("/tasks");
}

export function createTask(task: NewTask): Promise<Task> {
  return json<Task>("/tasks", { method: "POST", body: JSON.stringify(task) });
}

export function updateTask(id: number, task: Partial<NewTask>): Promise<Task> {
  return json<Task>(`/tasks/${id}`, { method: "PATCH", body: JSON.stringify(task) });
}

export function deleteTask(id: number): Promise<void> {
  return json<void>(`/tasks/${id}`, { method: "DELETE" });
}
