import { describe, expect, it } from "vitest";
import {
  buildFallbackContactSuggestions,
  canonicalAddrSpec,
  checkRecipientDraftLimits,
  cleanDisplayName,
  effectiveRecipientValues,
  hasTopLevelDelimiter,
  isDelimiterKeyAtEnd,
  isValidAddrSpec,
  MAX_MAILBOX_BYTES,
  MAX_RECIPIENTS_TOTAL,
  mergeRecipientTokens,
  parseRecipientList,
  parseRecipientToken,
  recipientDedupKey,
  splitRecipientTokens,
} from "../src/compose/recipients";

describe("compose recipient parsing, normalization, and limits", () => {
  it("splits comma, semicolon, full-width, newline, and tab delimiters while respecting quotes, escapes, and angle brackets", () => {
    const raw = [
      '"Doe, Alice" <alice@example.com>, "Bob \\"B\\"; Smith" <bob@example.com>;',
      "carol@example.com，dave@example.com；",
      "eve@example.com\tfrank@example.com",
      "grace@example.com\r\nhannah@example.com",
    ].join("\n");

    expect(splitRecipientTokens(raw)).toEqual([
      '"Doe, Alice" <alice@example.com>',
      '"Bob \\"B\\"; Smith" <bob@example.com>',
      "carol@example.com",
      "dave@example.com",
      "eve@example.com",
      "frank@example.com",
      "grace@example.com",
      "hannah@example.com",
    ]);
  });

  it("detects top-level delimiters accurately without breaking inside quotes or angle brackets", () => {
    expect(isDelimiterKeyAtEnd(",", '"Doe')).toBe(false);
    expect(isDelimiterKeyAtEnd(",", '"Doe, Alice" <alice@x.com')).toBe(false);
    expect(isDelimiterKeyAtEnd(",", '"Doe, Alice" <alice@x.com>')).toBe(true);
    expect(isDelimiterKeyAtEnd("，", "alice@x.com")).toBe(true);
    expect(isDelimiterKeyAtEnd("；", "alice@x.com")).toBe(true);
    expect(hasTopLevelDelimiter('"Doe, Alice" <alice@x.com>')).toBe(false);
    expect(hasTopLevelDelimiter('"Doe, Alice" <alice@x.com>, bob@x.com')).toBe(true);
    expect(hasTopLevelDelimiter("a@x.com\tb@y.com")).toBe(true);
  });

  it("normalizes display-name angle forms to bare addr-spec and extracts clean plain-text display names", () => {
    expect(parseRecipientToken('"Doe, Alice" <Alice@EXAMPLE.COM>')).toEqual({
      value: "Alice@example.com",
      displayName: "Doe, Alice",
      valid: true,
    });
    expect(parseRecipientToken("  <bob@Sub.Example.ORG>  ")).toEqual({
      value: "bob@sub.example.org",
      valid: true,
    });
    expect(parseRecipientToken("alice@")).toEqual({
      value: "alice@",
      valid: false,
    });
    expect(cleanDisplayName("  Alice\u0007  Smith  ", "alice@x.com")).toBe("Alice Smith");
    expect(cleanDisplayName("alice@x.com", "alice@x.com")).toBeUndefined();
  });

  it("preserves local-part original casing while deduplicating domain case-insensitively", () => {
    expect(canonicalAddrSpec("Alice.Dev@EXAMPLE.COM")).toBe("Alice.Dev@example.com");
    const parsed = parseRecipientList(
      "Alice@EXAMPLE.COM, Alice@example.com, alice@example.com",
    );
    expect(parsed.map((p) => p.value)).toEqual([
      "Alice@example.com",
      "alice@example.com",
    ]);
    expect(recipientDedupKey("Alice@Example.COM")).toBe("Alice@example.com");
  });

  it("matches maild_client::validate_send_field rules for valid and invalid addresses", () => {
    expect(isValidAddrSpec("user+tag@example.com")).toBe(true);
    expect(isValidAddrSpec("a@b")).toBe(true);
    expect(isValidAddrSpec("alice@")).toBe(false);
    expect(isValidAddrSpec("@example.com")).toBe(false);
    expect(isValidAddrSpec(".alice@example.com")).toBe(false);
    expect(isValidAddrSpec("alice.@example.com")).toBe(false);
    expect(isValidAddrSpec("a..b@example.com")).toBe(false);
    expect(isValidAddrSpec("alice@-example.com")).toBe(false);
    expect(isValidAddrSpec("alice@example-.com")).toBe(false);
    expect(isValidAddrSpec("alice@example..com")).toBe(false);
    expect(isValidAddrSpec("测试@example.com")).toBe(false);
    expect(isValidAddrSpec("alice\u0000@example.com")).toBe(false);
    expect(isValidAddrSpec(`${"a".repeat(MAX_MAILBOX_BYTES)}@b.com`)).toBe(false);
  });

  it("merges in-place edits while preserving exact chip order and deduplicating within the field", () => {
    const existing = ["first@x.com", "second@x.com", "third@x.com"];
    const edited = mergeRecipientTokens(
      existing,
      parseRecipientList("updated@X.COM"),
      1,
    );
    expect(edited.chips).toEqual(["first@x.com", "updated@x.com", "third@x.com"]);

    // Expanding an in-place edit into multiple recipients keeps surrounding order.
    const expanded = mergeRecipientTokens(
      existing,
      parseRecipientList("two-a@x.com, two-b@x.com"),
      1,
    );
    expect(expanded.chips).toEqual([
      "first@x.com",
      "two-a@x.com",
      "two-b@x.com",
      "third@x.com",
    ]);

    // Clearing an in-place edit removes that slot cleanly.
    const cleared = mergeRecipientTokens(existing, [], 1);
    expect(cleared.chips).toEqual(["first@x.com", "third@x.com"]);

    // Effective list includes pending uncommitted text without dropping chips.
    expect(
      effectiveRecipientValues({
        chips: ["first@x.com"],
        pending: "Bob <bob@X.COM>",
        editingIndex: null,
        editingOriginal: null,
      }),
    ).toEqual(["first@x.com", "bob@x.com"]);
  });

  it("flags >100 total recipients, >320 byte entries, and control chars without truncating", () => {
    const fifty = Array.from({ length: 50 }, (_, i) => `u${i}@example.com`);
    const fiftyOne = Array.from({ length: 51 }, (_, i) => `c${i}@example.com`);
    expect(
      checkRecipientDraftLimits({ to: fifty, cc: fiftyOne, bcc: [] }),
    ).toEqual({ cc: "too_many" });

    const exactlyHundred = Array.from(
      { length: MAX_RECIPIENTS_TOTAL },
      (_, i) => `u${i}@example.com`,
    );
    expect(
      checkRecipientDraftLimits({ to: exactlyHundred, cc: [], bcc: [] }),
    ).toEqual({});

    const overlong = `${"x".repeat(315)}@example.com`;
    expect(
      checkRecipientDraftLimits({ to: ["ok@example.com"], cc: [], bcc: [overlong] }),
    ).toEqual({ bcc: "too_long" });

    expect(
      checkRecipientDraftLimits({ to: ["bad\u0007@example.com"], cc: [], bcc: [] }),
    ).toEqual({ to: "control_char" });
  });

  it("builds current-account fallback contact suggestions with strict account isolation", () => {
    const rows = [
      {
        account: "work",
        sender: '"Alice Chen" <alice@work.test>',
        recipient: "me@work.test",
        cc: "bob@work.test",
        sort_at: 200,
      },
      {
        account: "personal",
        sender: '"Secret Friend" <secret@personal.test>',
        recipient: "me@personal.test",
        sort_at: 999,
      },
    ];
    const senders = [
      { account: "work", address: "me@work.test", label: "Work Me" },
      { account: "personal", address: "me@personal.test", label: "Personal" },
    ];

    const allWork = buildFallbackContactSuggestions("work", rows, senders, "", 8);
    expect(allWork.map((c) => c.address)).not.toContain("secret@personal.test");
    expect(allWork[0]).toEqual({ address: "alice@work.test", name: "Alice Chen" });
    // Self alias is ranked below correspondents.
    expect(allWork.at(-1)?.address).toBe("me@work.test");

    // Matches by display name as well as address.
    const byName = buildFallbackContactSuggestions("work", rows, senders, "chen", 8);
    expect(byName).toEqual([{ address: "alice@work.test", name: "Alice Chen" }]);
  });
});
