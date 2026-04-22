import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import Crews from "./pages/Crews";
import CrewEditor from "./pages/CrewEditor";
import Debug from "./pages/Debug";

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<Navigate to="/crews" replace />} />
        <Route path="/crews" element={<Crews />} />
        <Route path="/crews/:crewId" element={<CrewEditor />} />
        {/* Scratch page for C6 PTY validation — remove when C10 lands. */}
        <Route path="/debug" element={<Debug />} />
      </Routes>
    </BrowserRouter>
  );
}
