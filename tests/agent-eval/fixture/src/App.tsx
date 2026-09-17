import { Link } from "react-router-dom";

import { Routes } from "./routes";

export function App() {
  return (
    <div className="layout">
      <nav className="nav">
        <Link to="/">Tasks</Link>
        <Link to="/done">Done</Link>
      </nav>
      <Routes />
    </div>
  );
}
