// The board animation: tic-tac-toe turns, drawn with SVG. Marks are drawn in
// turns; `once` plays it a single time ending on a three-in-a-row line
// (welcome screen), otherwise it loops (the Working… line). With reduced
// motion it shows the board still.
import type { ReactNode } from "react";

const cell = (i: number): [number, number] => [15 + 30 * (i % 3), 15 + 30 * Math.floor(i / 3)];

export function Board({ once = false, grid = true }: { once?: boolean; grid?: boolean }) {
  const still = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const dur = once ? 2.8 : 3.2;
  const at = (t: number) => (t / dur).toFixed(3);
  // Draw-in: dashoffset from the full length to 0 at time t; looping marks
  // then clear together near the end of the cycle.
  const draw = (len: number, t: number, d = 0.25): ReactNode =>
    still ? null : (
      <animate
        attributeName="stroke-dashoffset"
        dur={`${dur}s`}
        {...(once ? { fill: "freeze" } : { repeatCount: "indefinite" })}
        values={`${len};${len};0${once ? "" : `;0;${len}`}`}
        keyTimes={`0;${at(t)};${at(t + d)}${once ? "" : ";0.9;1"}`}
      />
    );
  const dash = (len: number) => (still ? {} : { strokeDasharray: `${len}`, strokeDashoffset: `${len}` });
  const parts: ReactNode[] = [];
  if (grid)
    [
      [30, 6, 30, 84],
      [60, 6, 60, 84],
      [6, 30, 84, 30],
      [6, 60, 84, 60],
    ].forEach(([x1, y1, x2, y2], i) =>
      parts.push(
        <line
          key={`g${i}`}
          x1={x1}
          y1={y1}
          x2={x2}
          y2={y2}
          style={{ stroke: `var(${once ? "--line" : "--focus"})` }}
          strokeWidth={once ? 3 : 5}
          opacity={once ? 1 : 0.6}
          strokeLinecap="round"
          {...(once ? dash(78) : {})}
        >
          {once ? draw(78, i * 0.1, 0.3) : null}
        </line>,
      ),
    );
  const t0 = once ? 0.7 : 0.1;
  const step = once ? 0.3 : 0.45;
  (
    [
      ["x", 4],
      ["o", 0],
      ["x", 2],
      ["o", 8],
      ["x", 6],
    ] as const
  ).forEach(([mark, c], n) => {
    const [cx, cy] = cell(c);
    const t = t0 + n * step;
    if (mark === "o")
      parts.push(
        <circle
          key={`m${n}`}
          cx={cx}
          cy={cy}
          r={8.5}
          fill="none"
          style={{ stroke: "var(--ink)" }}
          strokeWidth={4.5}
          transform={`rotate(-90 ${cx} ${cy})`}
          {...dash(53.4)}
        >
          {draw(53.4, t)}
        </circle>,
      );
    else
      [`M${cx - 7.5} ${cy - 7.5}L${cx + 7.5} ${cy + 7.5}`, `M${cx + 7.5} ${cy - 7.5}L${cx - 7.5} ${cy + 7.5}`].forEach((d, k) =>
        parts.push(
          <path
            key={`m${n}${k}`}
            d={d}
            fill="none"
            style={{ stroke: "var(--rust)" }}
            strokeWidth={5}
            strokeLinecap="round"
            {...dash(21.2)}
          >
            {draw(21.2, t + k * 0.12, 0.12)}
          </path>,
        ),
      );
  });
  if (once)
    parts.push(
      <line
        key="win"
        x1={80}
        y1={10}
        x2={10}
        y2={80}
        style={{ stroke: "var(--rust)" }}
        strokeWidth={3.5}
        strokeLinecap="round"
        opacity={0.85}
        {...dash(99)}
      >
        {draw(99, 2.3, 0.4)}
      </line>,
    );
  return (
    <svg viewBox="0 0 90 90" aria-hidden="true">
      {parts}
    </svg>
  );
}
