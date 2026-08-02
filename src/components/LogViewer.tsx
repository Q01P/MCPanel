import { type UIEvent, useEffect, useRef, useState } from "react";
import { useLogs } from "../logs";
import { usePanel } from "../store";
import type { LogEntry } from "../logs";

const ROW_HEIGHT = 20;
const VIEW_HEIGHT = 320;
const OVERSCAN = 10;
const EMPTY: LogEntry[] = [];

function Row({ entry }: { entry: LogEntry }) {
  if (entry.kind === "gap") {
    return <div className="log-row log-marker">{entry.text}</div>;
  }
  return (
    <div
      className={`log-row log-line${entry.stream === "stderr" ? " log-stderr" : ""}`}
      title={entry.text}
    >
      {entry.text}
    </div>
  );
}

/**
 * Hand-rolled fixed-row virtualization: only the visible slice (plus
 * overscan) is in the DOM; spacer divs keep the scrollbar honest. Follow-tail
 * pins to the bottom until the user scrolls up, and re-engages when they
 * scroll back down.
 */
export function LogViewer() {
  const selected = useLogs((s) => s.selected);
  const entries = useLogs((s) =>
    s.selected == null ? EMPTY : (s.byServer[s.selected] ?? EMPTY),
  );
  const laggedMissed = useLogs((s) => s.laggedMissed);
  const select = useLogs((s) => s.select);
  const server = usePanel((s) => s.servers.find((x) => x.id === selected));

  const scrollRef = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const [scrollTop, setScrollTop] = useState(0);

  useEffect(() => {
    if (follow && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [entries, follow]);

  useEffect(() => {
    // New selection: restart pinned to the tail.
    setFollow(true);
    setScrollTop(0);
  }, [selected]);

  if (selected == null) return null;

  const total = entries.length;
  const start = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const end = Math.min(
    total,
    Math.ceil((scrollTop + VIEW_HEIGHT) / ROW_HEIGHT) + OVERSCAN,
  );

  const onScroll = (event: UIEvent<HTMLDivElement>) => {
    const el = event.currentTarget;
    setScrollTop(el.scrollTop);
    setFollow(el.scrollHeight - el.scrollTop - el.clientHeight < ROW_HEIGHT * 2);
  };

  return (
    <section className="log-panel">
      <header className="log-header">
        <span className="log-title">
          logs · {server?.name ?? `server ${selected}`}
        </span>
        {laggedMissed > 0 && (
          <span className="log-lagged" title="The UI event stream fell behind">
            ⚠ {laggedMissed} events missed
          </span>
        )}
        {!follow && (
          <button className="log-follow" onClick={() => setFollow(true)}>
            ⤓ follow
          </button>
        )}
        <button
          className="log-close"
          aria-label="close logs"
          onClick={() => select(null)}
        >
          ×
        </button>
      </header>
      <div
        className="log-scroll"
        style={{ height: VIEW_HEIGHT }}
        ref={scrollRef}
        onScroll={onScroll}
      >
        {total === 0 ? (
          <p className="empty">no output yet</p>
        ) : (
          <>
            <div style={{ height: start * ROW_HEIGHT }} />
            {entries.slice(start, end).map((entry) => (
              <Row key={entry.seq} entry={entry} />
            ))}
            <div style={{ height: (total - end) * ROW_HEIGHT }} />
          </>
        )}
      </div>
    </section>
  );
}
