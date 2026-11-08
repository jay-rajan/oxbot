// The owner's row under a finished reply: thumbs and "Test case…".
// Drawn by the chat inside a turn_done line; nothing for anyone else.
import { useApp, useSel } from "../../app/context";
import { local } from "../../state/store";
import { comparable, isOwner } from "./devTools";

export function RateRow({ bot, seq }: { bot: string; seq: number }) {
  const { send, dispatch } = useApp();
  const owner = useSel((s) => isOwner(s.connection.settings));
  const info = useSel((s) => s.bots.byId[bot]);
  const r = useSel((s) => s.cards.rated[bot]);
  if (!seq || !owner || !comparable(info)) return null;
  const up = r?.ratings?.[seq];
  const kept = !!r?.cases?.includes(seq);
  return (
    <span className="rate-row">
      {(
        [
          ["👍", true, "Good reply"],
          ["👎", false, "Bad reply"],
        ] as const
      ).map(([label, value, title]) => (
        <button
          key={label}
          type="button"
          title={title}
          aria-pressed={up === value}
          onClick={() => send({ type: "rate_turn", bot, seq, up: up === value ? null : value })}
        >
          {label}
        </button>
      ))}
      <button
        type="button"
        title={kept ? "Kept as a test case: see Compare in the bot's menu" : "Keep this task to replay on other models"}
        onClick={() =>
          kept
            ? dispatch(local("ui/open", { dialog: "compare", arg: `${bot}:cmpCases` }))
            : send({ type: "draft_test_case", bot, seq })
        }
      >
        {kept ? "✓ test case" : "Test case…"}
      </button>
    </span>
  );
}
