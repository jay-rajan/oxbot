// Who a bot is shared with, and what that means, in words.
import type { BotInfo, Sharing } from "../../net/protocol";

/** Who a bot is shared with, as its settings show it. */
export function sharingOf(b: BotInfo): Sharing {
  return b.sharing || (b.private ? "me" : "everyone");
}

// Personal details are always swapped for stand-ins before anything is sent.
const PERSONAL =
  " Personal details are swapped for stand-ins before anything is sent; details only described in words, and names oxbot was never told, aren't.";

/** What sharing a bot this way means, for the hint under the choice. */
export function privacyHint(sharing: Sharing | boolean, b: BotInfo): string {
  const s: Sharing = sharing === true ? "me" : sharing === false ? "everyone" : sharing;
  const maker = b.created_by || "its maker";
  if (s === "people")
    return `Only ${maker}, the people picked and the owner see it, its chat and its reports, give it tasks and answer its approval requests. It acts with the websites ${maker} is signed in to, in ${maker}'s own browser, whoever asks it; only ${maker} signs it in to websites.${PERSONAL}`;
  return (
    (s === "me"
      ? `Only ${maker} and the owner see it, its chat and its reports. ${maker}'s own Assistant can give it tasks; it asks ${maker} first if it has read web content or other bots' replies. It uses the websites ${maker} is signed in to, never the home's.`
      : "Everyone in the home sees and uses it, its chat and its reports. It uses the websites the home is signed in to.") + PERSONAL
  );
}
