import type { AccountProfile } from "./types";

export function accountLabel(profile: Pick<AccountProfile, "display_name" | "address">): string {
  return profile.display_name.trim() || profile.address.trim();
}
