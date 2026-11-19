import { describe, expect, it } from "vitest";
import type { AppSettings, BotInfo } from "../../net/protocol";
import { mayKeepPrivate, ownsHome } from "./sharing";

const set = (admin: boolean, you = "Ann") => ({ admin, you, owner: false, owner_name: "Jay" }) as AppSettings;
const bot = (created_by: string | null) => ({ id: "b", created_by }) as unknown as BotInfo;

describe("only administrators keep bots private", () => {
  it("ownsHome follows settings.admin", () => {
    expect(ownsHome(set(true))).toBe(true);
    expect(ownsHome(set(false))).toBe(false);
    expect(ownsHome(null)).toBe(false);
  });
  it("an administrator may keep their own bots (or unknown makers') private", () => {
    expect(mayKeepPrivate(bot("Ann"), set(true))).toBe(true);
    expect(mayKeepPrivate(bot(null), set(true))).toBe(true);
    expect(mayKeepPrivate(bot("Jay"), set(true))).toBe(false);
    expect(mayKeepPrivate(bot("Ann"), set(false))).toBe(false);
  });
});
