// Persistent shell: sidebar on the left, page content fills the rest.
//
// Used as a React Router layout route so the Sidebar mounts ONCE for the
// app's lifetime. Child routes render into `<Outlet />`. Without this,
// every page change tears down the sidebar and its `runner/activity`
// listener — and any event emitted during the brief reattach window
// (e.g., the activity event from `session_start_direct` triggered on
// the chat page's mount) gets lost, leaving the SESSION list stale.

import type { ReactNode } from "react";
import { Outlet } from "react-router-dom";

import { Sidebar } from "./Sidebar";

export function AppShell({ children }: { children?: ReactNode }) {
  return (
    <div className="flex h-screen overflow-hidden bg-bg text-fg">
      <Sidebar />
      <main className="relative flex flex-1 flex-col overflow-hidden">
        <div
          data-tauri-drag-region
          className="pointer-events-auto absolute left-0 right-0 top-0 z-10 h-7"
        />
        {children ?? <Outlet />}
      </main>
    </div>
  );
}
