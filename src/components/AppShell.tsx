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
      <main className="relative flex flex-1 flex-col overflow-hidden">
        {/* Tauri drag strip across the top of the content pane — same
            pattern as quill (every page paints its own strip). The
            sidebar already has its own drag region; this covers the
            content half so the user can grab the window from anywhere
            along the title-bar row. h-7 matches the macOS title bar so
            it overlays the empty space above page headers without
            clipping any controls. */}
        <div
          data-tauri-drag-region
          className="pointer-events-auto absolute left-0 right-0 top-0 z-10 h-7"
        />
        {children}
      </main>
    </div>
  );
}
