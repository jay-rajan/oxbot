// Who a bot is shared with, and who may keep bots private.
import type { AppSettings, BotInfo, Sharing } from "../../net/protocol";

// Only the home's administrators have private bots (the owner of the home
// computer also has private sign-ins); everyone else's bots are shared. The
// server says who's an administrator, and refuses anyone else.
export function ownsHome(settings: AppSettings | null | undefined): boolean {
  return !!settings?.admin;
}

/** Whether an administrator may make bot `b` private: only their own (or one
 *  nobody is recorded as making). */
export function mayKeepPrivate(b: BotInfo, settings: AppSettings | null | undefined): boolean {
  return ownsHome(settings) && (!b.created_by || b.created_by === settings?.you);
}

/** Who a bot is shared with, as its settings show it. */
export function sharingOf(b: BotInfo): Sharing {
  return b.sharing || (b.private ? "me" : "everyone");
}
