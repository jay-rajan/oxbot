// The line under a bot's name: what it's for, in a few words, from its
// instructions (the first sentence, cut at a word).

const MAX = 60;

/** The first sentence of `instructions`, at most about 60 characters. */
export function purposeLine(instructions: string | null | undefined): string {
  const first =
    (instructions ?? "")
      .split("\n")
      .map((l) => l.replace(/^\s*(#+|[-*]|\d+[.)])\s*/, "").trim())
      .find((l) => l.length > 0) ?? "";
  const sentence = first.match(/^.*?[.!?](?=\s|$)/)?.[0] ?? first;
  if (sentence.length <= MAX) return sentence;
  const cut = sentence.slice(0, MAX);
  const space = cut.lastIndexOf(" ");
  return `${(space > MAX / 2 ? cut.slice(0, space) : cut).replace(/[,;:]$/, "")}…`;
}
