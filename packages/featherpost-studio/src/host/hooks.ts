import { useSyncExternalStore } from 'react';

import type { Store } from './store';

export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

export function useExternal<T>(subscribe: (listener: () => void) => () => void, getSnapshot: () => T): T {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
