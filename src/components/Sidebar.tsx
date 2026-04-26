// App sidebar — 240px, matches runners-design.pen "Sidebar" frame.
//
// Order mirrors the design's Sidebar frame (Runner / Crew / Mission). Runner
// and Mission are placeholders until their pages land — runners get a
// top-level home with v0.x, missions ship in C11. Showing them disabled now
// lets users see the shape the shell is growing into without dead links.

import { NavLink } from "react-router-dom";

type NavItem = {
  to: string;
  label: string;
  enabled: boolean;
  hint?: string;
};

const NAV: NavItem[] = [
  { to: "/runners", label: "Runner", enabled: true },
  { to: "/crews", label: "Crew", enabled: true },
  { to: "/missions", label: "Mission", enabled: false, hint: "Coming with C11" },
];

export function Sidebar() {
  return (
    <aside className="flex h-full w-60 shrink-0 select-none flex-col border-r border-[#E5E5E5] bg-[#F2F2F2]">
      {/* Tauri drag region — height matches the stock macOS title bar
          space. Lets the user drag the window from empty sidebar area. */}
      <div data-tauri-drag-region className="h-6" />

      <div className="flex flex-col gap-1 px-5 pb-5">
        <div className="mb-4 flex items-center px-1 text-[18px] font-semibold tracking-tight text-neutral-900">
          runner
        </div>
        {NAV.map((item) =>
          item.enabled ? (
            <NavLink
              key={item.to}
              to={item.to}
              className={({ isActive }) =>
                `rounded-md px-2.5 py-2 text-sm transition-colors ${
                  isActive
                    ? "bg-[#E5E5E5] font-semibold text-neutral-900"
                    : "text-neutral-600 hover:bg-[#E5E5E5]/60"
                }`
              }
            >
              {item.label}
            </NavLink>
          ) : (
            <span
              key={item.to}
              title={item.hint}
              aria-disabled="true"
              className="cursor-not-allowed rounded-md px-2.5 py-2 text-sm text-neutral-400"
            >
              {item.label}
            </span>
          ),
        )}
      </div>
    </aside>
  );
}
