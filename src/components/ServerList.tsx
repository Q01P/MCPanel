import { usePanel } from "../store";
import type { ServerOverview, ServerStatus } from "../types";

function statusLabel(status: ServerStatus): string {
  return status.state === "errored" ? "error" : status.state;
}

function StatusBadge({ status }: { status: ServerStatus }) {
  return (
    <span
      className={`badge badge-${status.state}`}
      title={status.state === "errored" ? status.message : undefined}
    >
      {statusLabel(status)}
    </span>
  );
}

function ServerRow({ server }: { server: ServerOverview }) {
  const toggle = usePanel((s) => s.toggle);
  const remove = usePanel((s) => s.remove);

  const running = server.status.state === "running";
  const busy =
    server.status.state === "starting" || server.status.state === "initializing";

  return (
    <li className="server-row">
      <div className="server-info">
        <span className="server-name">{server.name}</span>
        <span className="server-command">
          {server.command} {server.args.join(" ")}
        </span>
      </div>
      <StatusBadge status={server.status} />
      <label className={`switch${busy ? " switch-busy" : ""}`}>
        <input
          type="checkbox"
          checked={running || busy}
          disabled={busy}
          onChange={(e) => void toggle(server.id, e.target.checked)}
        />
        <span className="slider" />
      </label>
      <button
        className="remove-button"
        title="Remove server"
        onClick={() => void remove(server.id)}
      >
        remove
      </button>
    </li>
  );
}

export function ServerList() {
  const servers = usePanel((s) => s.servers);
  const loaded = usePanel((s) => s.loaded);

  if (!loaded) return <p className="empty">loading…</p>;
  if (servers.length === 0)
    return <p className="empty">No servers configured yet — add one below.</p>;

  return (
    <ul className="server-list">
      {servers.map((server) => (
        <ServerRow key={server.id} server={server} />
      ))}
    </ul>
  );
}
