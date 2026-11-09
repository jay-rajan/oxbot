// Comparing models is a developer tool: only the owner, and only with
// developer tools on; never for the Assistant or someone else's private bot.
import type { AppSettings, BotInfo } from "../../net/protocol";

export const isOwner = (settings: AppSettings | null) => !!(settings?.owner && settings.dev_tools);
export const comparable = (b: BotInfo | undefined) => !!(b && !b.central && !b.locked);
