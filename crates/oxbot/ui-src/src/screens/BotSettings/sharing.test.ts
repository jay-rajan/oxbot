import { describe, expect, it } from "vitest";
import type { AppSettings, BotInfo } from "../../net/protocol";
import { mayKeepPrivate, ownsHome } from "./sharing";

const set = (owner: boolean) => ({ owner, owner_name: "Jay" }) as AppSettings;
const bot = (created_by: string | null) => ({ id: "b", created_by }) as unknown as BotInfo;

describe("only the owner keeps bots private", () => {
  it("ownsHome follows settings.owner", () => {
    expect(ownsHome(set(true))).toBe(true);
    expect(ownsHome(set(false))).toBe(false);
    expect(ownsHome(null)).toBe(false);
  });
  it("the owner may keep their own bots (or unknown makers') private", () => {
    expect(mayKeepPrivate(bot("Jay"), set(true))).toBe(true);
    expect(mayKeepPrivate(bot(null), set(true))).toBe(true);
    expect(mayKeepPrivate(bot("Sam"), set(true))).toBe(false);
    expect(mayKeepPrivate(bot("Jay"), set(false))).toBe(false);
  });
});
