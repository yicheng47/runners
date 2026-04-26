// In-process registry of live direct-chat sessions, keyed by runner
// handle. Lets the sidebar's SESSION list re-attach to a running PTY
// instead of spawning a second one.
//
// Why a module-scoped Map and not a backend query: in v0 direct chats
// are ephemeral (no transcript persistence per the C8.5 plan), so the
// only thing we need to remember across navigation is the in-flight
// session id. State is lost on app reload — that matches the v0
// "ephemeral chat" contract; the backend session keeps running until
// killed, but if you reload the window you'll lose the link to it.
//
// A future C10/C11 improvement would expose a backend query
// `session_list_running()` and replace this store with a live
// projection — same shape as runner/activity events but with the
// session ids included.

type Listener = () => void;

const handleToSessionId = new Map<string, string>();
const listeners = new Set<Listener>();

export function setActiveSession(handle: string, sessionId: string): void {
  handleToSessionId.set(handle, sessionId);
  for (const l of listeners) l();
}

export function clearActiveSession(handle: string): void {
  if (handleToSessionId.delete(handle)) {
    for (const l of listeners) l();
  }
}

export function getActiveSession(handle: string): string | null {
  return handleToSessionId.get(handle) ?? null;
}

export function subscribeActiveSessions(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
