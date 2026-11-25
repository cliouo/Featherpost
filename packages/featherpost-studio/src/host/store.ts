/** 最小的外部 store：`useSyncExternalStore` 直接吃它，不引状态库。 */
export class Store<T> {
  private listeners = new Set<() => void>();
  constructor(private value: T) {}
  getSnapshot = () => this.value;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  set(next: T) {
    if (Object.is(next, this.value)) return;
    this.value = next;
    for (const listener of this.listeners) listener();
  }
  patch(partial: Partial<T>) {
    this.set({ ...this.value, ...partial });
  }
}
