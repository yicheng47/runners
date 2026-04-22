// Persistent shell: sidebar on the left, page content fills the rest.
//
// Matches design/runners-design.pen (frames Crews/Crew Detail/Missions all
// wrap the same sidebar). Quill uses the same pattern — Sidebar + main
// sibling inside a flex row, with a Tauri drag region spanning the top of
// the sidebar so the user can drag the window from empty space.

import type { ReactNode } from "react";

import { Sidebar } from "./Sidebar";

export function AppShell({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-screen overflow-hidden bg-[#FAFAFA] text-neutral-900">
      <Sidebar />
      <main className="flex flex-1 flex-col overflow-hidden">{children}</main>
    </div>
  );
}
