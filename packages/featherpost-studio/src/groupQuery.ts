import type {
  MailBridge,
  UiPage,
  UiSummary,
} from "../../maild-ui/src/bridge/types";
import type { MailboxQuery } from "../../maild-ui/src/mailbox/types";

interface Source {
  account: string;
  messages: UiSummary[];
  index: number;
  loaded: number;
  total: number;
}
interface Merge {
  key: string;
  sources: Source[];
  offset: number;
  initialized: boolean;
}
const PAGE_SIZE = 50;

/**
 * Merge native, independently paginated account streams, not the currently visible
 * global page. No new daemon protocol fields or APIs are required. Streams retain
 * their native order for equal timestamps; cross-account ties use account ID.
 */
export function groupedBridge(bridge: MailBridge): MailBridge {
  let current: Merge | null = null;
  async function query(q: MailboxQuery, offset: number): Promise<UiPage> {
    const { accounts, ...nativeQuery } = q;
    if (accounts === undefined) {
      current = null;
      return bridge.mailbox.query(nativeQuery, offset);
    }
    const key = JSON.stringify(q);
    if (
      !current ||
      current.key !== key ||
      current.offset !== offset ||
      offset === 0
    ) {
      current = {
        key,
        sources: [...new Set(accounts)].map((account) => ({
          account,
          messages: [],
          index: 0,
          loaded: 0,
          total: 0,
        })),
        offset: 0,
        initialized: false,
      };
    }
    const merge = current;
    const valid = () => {
      if (current !== merge) throw new Error("group query superseded");
    };
    const load = async (source: Source) => {
      valid();
      const page = await bridge.mailbox.query(
        { ...nativeQuery, account: source.account },
        source.loaded,
      );
      valid();
      if (page.messages.some((row) => row.account !== source.account))
        throw new Error("group account mismatch");
      if (!page.messages.length && source.loaded < page.total)
        throw new Error("empty incomplete account page");
      source.messages = page.messages;
      source.index = 0;
      source.loaded += page.messages.length;
      source.total = page.total;
    };
    if (!merge.initialized) {
      // Bound native IPC concurrency even when somebody connects hundreds of accounts.
      for (let n = 0; n < merge.sources.length; n += 4)
        await Promise.all(merge.sources.slice(n, n + 4).map(load));
      merge.initialized = true;
    }
    const messages: UiSummary[] = [];
    const end = offset + PAGE_SIZE;
    while (merge.offset < end) {
      valid();
      let chosen: Source | undefined;
      for (const source of merge.sources) {
        if (
          source.index === source.messages.length &&
          source.loaded < source.total
        )
          await load(source);
        const head = source.messages[source.index];
        if (!head) continue;
        const previous = chosen?.messages[chosen.index];
        if (
          !previous ||
          head.sort_at > previous.sort_at ||
          (head.sort_at === previous.sort_at &&
            source.account < chosen!.account)
        )
          chosen = source;
      }
      if (!chosen) break;
      const row = chosen.messages[chosen.index++];
      if (merge.offset >= offset) messages.push(row);
      merge.offset++;
    }
    return {
      messages,
      total: merge.sources.reduce((n, source) => n + source.total, 0),
    };
  }
  // Keep class-based bridges' prototype methods and their original receiver too.
  const mailbox = new Proxy(bridge.mailbox, {
    get(target, key) {
      if (key === "query") return query;
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  return new Proxy(bridge, {
    get(target, key) {
      if (key === "mailbox") return mailbox;
      if (key === "messages")
        return (...args: Parameters<MailBridge["messages"]>) => {
          current = null;
          return bridge.messages(...args);
        };
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}
