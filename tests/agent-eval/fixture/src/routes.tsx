import { Route, Routes as RouterRoutes } from "react-router-dom";

import { DonePage } from "./pages/Done";
import { TasksPage } from "./pages/Tasks";

export function Routes() {
  return (
    <RouterRoutes>
      <Route path="/" element={<TasksPage />} />
      <Route path="/done" element={<DonePage />} />
    </RouterRoutes>
  );
}
