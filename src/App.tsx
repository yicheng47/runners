import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import Crews from "./pages/Crews";
import CrewEditor from "./pages/CrewEditor";

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<Navigate to="/crews" replace />} />
        <Route path="/crews" element={<Crews />} />
        <Route path="/crews/:crewId" element={<CrewEditor />} />
      </Routes>
    </BrowserRouter>
  );
}
