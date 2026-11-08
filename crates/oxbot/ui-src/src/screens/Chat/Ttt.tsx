// The board animation for the "Working…" line: tic-tac-toe marks drawn in
// turns, looping (SVG, as the hand-written page drew it). With reduced
// motion it shows the board still. Colors are set with React styles: the
// page's security policy blocks inline style attributes in markup.
const CELL = (i: number) => [15 + 30 * (i % 3), 15 + 30 * Math.floor(i / 3)] as const;
const DUR = 3.2;

function Draw({ len, t, d = 0.25 }: { len: number; t: number; d?: number }) {
  const at = (x: number) => (x / DUR).toFixed(3);
  return (
    <animate
      attributeName="stroke-dashoffset"
      dur={`${DUR}s`}
      repeatCount="indefinite"
      values={`${len};${len};0;0;${len}`}
      keyTimes={`0;${at(t)};${at(t + d)};0.9;1`}
    />
  );
}

export function TttBoard() {
  const still = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const dash = (len: number) => (still ? {} : { strokeDasharray: len, strokeDashoffset: len });
  const grid: [number, number, number, number][] = [[30, 6, 30, 84], [60, 6, 60, 84], [6, 30, 84, 30], [6, 60, 84, 60]];
  const marks: ["x" | "o", number][] = [["x", 4], ["o", 0], ["x", 2], ["o", 8], ["x", 6]];
  return (
    <svg viewBox="0 0 90 90" aria-hidden="true">
      {grid.map(([x1, y1, x2, y2]) => (
        <line key={`${x1}-${y1}`} x1={x1} y1={y1} x2={x2} y2={y2} style={{ stroke: "var(--focus)" }} strokeWidth={5} opacity={0.6} strokeLinecap="round" />
      ))}
      {marks.map(([mark, cell], n) => {
        const [cx, cy] = CELL(cell);
        const t = 0.1 + n * 0.45;
        if (mark === "o")
          return (
            <circle key={n} cx={cx} cy={cy} r={8.5} fill="none" style={{ stroke: "var(--ink)" }} strokeWidth={4.5} transform={`rotate(-90 ${cx} ${cy})`} {...dash(53.4)}>
              {still ? null : <Draw len={53.4} t={t} />}
            </circle>
          );
        return [`M${cx - 7.5} ${cy - 7.5}L${cx + 7.5} ${cy + 7.5}`, `M${cx + 7.5} ${cy - 7.5}L${cx - 7.5} ${cy + 7.5}`].map((d, k) => (
          <path key={`${n}-${k}`} d={d} fill="none" style={{ stroke: "var(--rust)" }} strokeWidth={5} strokeLinecap="round" {...dash(21.2)}>
            {still ? null : <Draw len={21.2} t={t + k * 0.12} d={0.12} />}
          </path>
        ));
      })}
    </svg>
  );
}
