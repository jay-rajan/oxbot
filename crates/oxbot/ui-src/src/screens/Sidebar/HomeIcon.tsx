// The Home bots' icons (a calendar page, a plate and fork, a cart, a note), each in
// its own color: in the sidebar's Home row, and in the Calendar's, Family
// Notes' and their chats' headers, so each looks the same everywhere.
const icon = (d: React.ReactNode) => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
    {d}
  </svg>
);
/** The Home row's icons: a calendar page, a plate and fork, a note. */
export const HOME_ICONS: Record<string, React.ReactNode> = {
  "home-calendar": icon(
    <>
      <rect x="3" y="5" width="18" height="16" rx="2" />
      <path d="M3 10h18M8 3v4M16 3v4" />
    </>,
  ),
  "home-meals": icon(
    <>
      <circle cx="13" cy="13" r="7" />
      <circle cx="13" cy="13" r="3.5" />
      <path d="M4 3v6M2.5 3v4a1.5 1.5 0 0 0 3 0V3M4 9v12" />
    </>,
  ),
  "home-groceries": icon(
    <>
      <path d="M3 4h2l2.2 10.2a1.5 1.5 0 0 0 1.5 1.2h8.6a1.5 1.5 0 0 0 1.5-1.1L20.5 8H6.1" />
      <circle cx="9.5" cy="19.5" r="1.3" />
      <circle cx="16.5" cy="19.5" r="1.3" />
    </>,
  ),
  "home-notes": icon(
    <>
      <path d="M5 3h10l4 4v14H5z" />
      <path d="M15 3v4h4M8 12h8M8 16h6" />
    </>,
  ),
};

/** A Home bot's icon, or nothing for any other bot. It moves while its bot works. */
export function HomeIcon({ id, working }: { id: string; working?: boolean }) {
  const svg = HOME_ICONS[id];
  return svg ? (
    <span className={working ? "avatar home-icon working" : "avatar home-icon"} data-home={id} aria-hidden="true">
      {svg}
    </span>
  ) : null;
}
