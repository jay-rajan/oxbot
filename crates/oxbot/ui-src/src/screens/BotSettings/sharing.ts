// Who a bot is shared with, and who may keep bots private.
import type { AppSettings, BotInfo, Sharing } from "../../net/protocol";

// Only the owner of the home computer has private bots and private sign-ins;
// everyone else's bots are shared and use the home browser (the server says
// who the owner is, and refuses anyone else).
export function ownsHome(settings: AppSettings | null | undefined): boolean {
  return !!settings?.owner;
}

/** Whether the owner may make bot `b` private: only their own (or one nobody
 *  is recorded as making). */
export function mayKeepPrivate(b: BotInfo, settings: AppSettings | null | undefined): boolean {
  return ownsHome(settings) && (!b.created_by || b.created_by === settings?.owner_name);
}

/** Who a bot is shared with, as its settings show it. */
export function sharingOf(b: BotInfo): Sharing {
  return b.sharing || (b.private ? "me" : "everyone");
}
