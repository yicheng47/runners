// Runner Detail page (C8.5) — `/runners/:handle`.
//
// Mirrors design frame `ocAFJ`: two-column dark layout. Left holds the
// system-prompt block and "Crews using this runner" list; right holds
// big-number activity stat cards and an immutable Details panel. Header
// has the breadcrumb, runtime badge, and Edit / Chat now actions.

import { useCallback, useEffect, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";

import { listen } from "@tauri-apps/api/event";

import { api } from "../lib/api";
import type {
  CrewMembership,
  Runner,
  RunnerActivity,
  RunnerActivityEvent,
} from "../lib/types";
import { Button } from "../components/ui/Button";
import { RunnerEditDrawer } from "../components/RunnerEditDrawer";

export default function RunnerDetail() {
  const { handle: handleParam } = useParams<{ handle: string }>();
  const handle = handleParam ?? "";
  const navigate = useNavigate();

  const [runner, setRunner] = useState<Runner | null>(null);
  const [activity, setActivity] = useState<RunnerActivity | null>(null);
  const [crews, setCrews] = useState<CrewMembership[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [chatCwd, setChatCwd] = useState<string>("");
  const [openingChat, setOpeningChat] = useState(false);

  const refresh = useCallback(async () => {
    if (!handle) return;
    try {
      setError(null);
      const r = await api.runner.getByHandle(handle);
      setRunner(r);
      const [act, crewList] = await Promise.all([
        api.runner.activity(r.id),
        api.runner.crews(r.id),
      ]);
      setActivity(act);
      setCrews(crewList);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [handle]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<RunnerActivityEvent>("runner/activity", (event) => {
      if (event.payload.runner_id !== runner?.id) return;
      setActivity((prev) =>
        prev
          ? {
              ...prev,
              active_sessions: event.payload.active_sessions,
              active_missions: event.payload.active_missions,
              crew_count: event.payload.crew_count,
            }
          : prev,
      );
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [runner?.id]);

  const startChat = () => {
    if (!runner || openingChat) return;
    setOpeningChat(true);
    const cwd = chatCwd.trim() ? chatCwd.trim() : null;
    navigate(`/runners/${runner.handle}/chat`, {
      state: { runnerId: runner.id, cwd },
    });
  };

  return (
    <>
      <div className="flex-1 overflow-y-auto">
        <div className="mx-auto flex max-w-5xl flex-col gap-6 px-8 py-8">
          <header className="flex items-center justify-between gap-4">
            <div className="flex items-center gap-2 text-sm text-fg-2">
              <Link to="/runners" className="hover:text-fg">
                Runners
              </Link>
              <span className="text-line-strong">›</span>
              <span className="font-mono text-base font-semibold text-fg">
                @{handle}
              </span>
              {runner ? (
                <span className="rounded bg-raised px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-fg-2">
                  {runner.runtime}
                </span>
              ) : null}
            </div>
            <div className="flex items-center gap-2">
              <Button
                onClick={() => setEditing(true)}
                disabled={!runner}
                title="Edit runner"
              >
                Edit
              </Button>
              <Button
                variant="primary"
                onClick={() => void startChat()}
                disabled={!runner || openingChat}
                title="Start a one-on-one PTY with this runner"
              >
                {openingChat ? "Starting…" : "Chat now"}
              </Button>
            </div>
          </header>

          {runner?.display_name || runner?.role ? (
            <p className="text-sm text-fg-2">
              {runner.display_name}
              {runner.role ? (
                <>
                  <span className="text-line-strong"> · </span>
                  {runner.role}
                </>
              ) : null}
            </p>
          ) : null}

          {error ? (
            <div className="rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
              {error}
            </div>
          ) : null}

          {loading ? (
            <div className="text-sm text-fg-2">Loading…</div>
          ) : !runner ? (
            <div className="rounded border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
              Runner @{handle} not found.
            </div>
          ) : (
            <div className="grid grid-cols-3 gap-4">
              {/* Left column */}
              <div className="col-span-2 flex flex-col gap-4">
                <Card title="Default system prompt" subtitle="Used whenever this runner spawns. Override per crew/mission slot later (v0.x).">
                  {runner.system_prompt ? (
                    <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-fg">
                      {runner.system_prompt}
                    </pre>
                  ) : (
                    <p className="text-sm italic text-fg-3">
                      No system prompt set.
                    </p>
                  )}
                </Card>

                <Card title="Crews using this runner">
                  {crews.length === 0 ? (
                    <p className="text-sm italic text-fg-3">
                      Not in any crew yet. Add it to one from Crew Detail.
                    </p>
                  ) : (
                    <ul className="flex flex-col divide-y divide-line">
                      {crews.map((m) => (
                        <li
                          key={m.crew_id}
                          className="flex items-center justify-between py-2 text-sm"
                        >
                          <div className="flex items-center gap-2">
                            <span className="font-medium text-fg">
                              {m.crew_name}
                            </span>
                            {m.lead ? (
                              <span className="rounded bg-accent/10 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-accent">
                                LEAD
                              </span>
                            ) : null}
                          </div>
                          <Link
                            to={`/crews/${m.crew_id}`}
                            className="text-xs text-accent hover:underline"
                          >
                            Open →
                          </Link>
                        </li>
                      ))}
                    </ul>
                  )}
                </Card>

                <Card title="Chat now" subtitle="Spawn a one-on-one PTY. Direct chats don't join any mission's coordination bus.">
                  <div className="flex flex-col gap-2">
                    <label className="flex flex-col gap-1 text-xs text-fg-2">
                      Working directory (optional)
                      <input
                        value={chatCwd}
                        onChange={(e) => setChatCwd(e.target.value)}
                        placeholder={runner.working_dir ?? "/Users/you/projects/foo"}
                        className="rounded border border-line-strong bg-bg p-1.5 font-mono text-xs text-fg placeholder:text-fg-3 focus:border-fg-3 focus:outline-none"
                      />
                    </label>
                    <p className="text-[11px] text-fg-3">
                      Defaults to the runner's own working directory if blank.
                    </p>
                  </div>
                </Card>
              </div>

              {/* Right column */}
              <div className="col-span-1 flex flex-col gap-4">
                <Card title="Activity">
                  <div className="grid grid-cols-2 gap-2">
                    <BigStat
                      label="sessions"
                      value={activity?.active_sessions ?? 0}
                      accent={(activity?.active_sessions ?? 0) > 0}
                    />
                    <BigStat
                      label="missions"
                      value={activity?.active_missions ?? 0}
                      accent={(activity?.active_missions ?? 0) > 0}
                    />
                  </div>
                  <div className="mt-3 flex flex-col gap-1 border-t border-line pt-3 text-xs text-fg-2">
                    <Row label="In crews" value={`${activity?.crew_count ?? 0}`} />
                    <Row
                      label="Last seen"
                      value={
                        activity?.last_started_at
                          ? new Date(activity.last_started_at).toLocaleString()
                          : "—"
                      }
                    />
                  </div>
                </Card>

                <Card title="Details">
                  <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 text-xs">
                    <dt className="text-fg-3">Handle</dt>
                    <dd className="font-mono text-fg">@{runner.handle}</dd>
                    <dt className="text-fg-3">Runtime</dt>
                    <dd className="text-fg">{runner.runtime}</dd>
                    <dt className="text-fg-3">Command</dt>
                    <dd className="break-all font-mono text-fg">
                      {runner.command}
                    </dd>
                    {runner.args.length > 0 ? (
                      <>
                        <dt className="text-fg-3">Args</dt>
                        <dd className="break-all font-mono text-fg">
                          {runner.args.join(" ")}
                        </dd>
                      </>
                    ) : null}
                    <dt className="text-fg-3">Created</dt>
                    <dd className="text-fg">
                      {new Date(runner.created_at).toLocaleString()}
                    </dd>
                    <dt className="text-fg-3">ID</dt>
                    <dd className="break-all font-mono text-[10px] text-fg-3">
                      {runner.id}
                    </dd>
                  </dl>
                </Card>
              </div>
            </div>
          )}
        </div>
      </div>

      <RunnerEditDrawer
        open={editing}
        runner={runner}
        onClose={() => setEditing(false)}
        onSaved={async () => {
          setEditing(false);
          await refresh();
        }}
      />
    </>
  );
}

function Card({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="flex flex-col gap-2 rounded-lg border border-line bg-panel p-4">
      <div className="flex flex-col gap-0.5">
        <h2 className="text-sm font-semibold text-fg">{title}</h2>
        {subtitle ? (
          <p className="text-[11px] text-fg-3">{subtitle}</p>
        ) : null}
      </div>
      <div>{children}</div>
    </section>
  );
}

function BigStat({
  label,
  value,
  accent,
}: {
  label: string;
  value: number;
  accent?: boolean;
}) {
  return (
    <div className="flex flex-col gap-0.5 rounded border border-line bg-bg p-3">
      <span
        className={`text-3xl font-bold leading-none ${accent ? "text-accent" : "text-fg"}`}
      >
        {value}
      </span>
      <span className="text-[10px] uppercase tracking-wider text-fg-3">
        {label}
      </span>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-fg-3">{label}</span>
      <span className="text-fg">{value}</span>
    </div>
  );
}
