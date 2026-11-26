// @vitest-environment jsdom
import { act, createElement as h, Fragment, useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import * as ts from "typescript";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { StudioSelect } from "../src/StudioSelect";
import { Choice, ChoiceRenderer, type ChoiceProps } from "../src/settings/Choice";
import { acquireFallbackLayer, closeAllPopovers } from "../src/ui";
import { isShortcutBlockedTarget } from "../src/Workspace";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(() => {
  if (typeof globalThis.ResizeObserver === "undefined") {
    globalThis.ResizeObserver = class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  if (typeof globalThis.PointerEvent === "undefined") {
    class PolyfillPointerEvent extends MouseEvent {
      readonly pointerId: number;
      readonly pointerType: string;
      constructor(type: string, params: PointerEventInit = {}) {
        super(type, params);
        this.pointerId = params.pointerId ?? 1;
        this.pointerType = params.pointerType ?? "mouse";
      }
    }
    globalThis.PointerEvent = PolyfillPointerEvent as unknown as typeof PointerEvent;
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
});

const mountedRoots: Array<{ root: Root; container: HTMLElement }> = [];

function mount(node: ReactNode) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  mountedRoots.push({ root, container });
  return {
    container,
    rerender(next: ReactNode) {
      act(() => {
        root.render(next);
      });
    },
    unmount() {
      act(() => {
        root.unmount();
      });
      container.remove();
    },
  };
}

function flushTimers() {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function keydown(target: EventTarget, key: string, init: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  act(() => {
    target.dispatchEvent(event);
  });
  return event;
}

function openListbox(): HTMLElement | null {
  return document.body.querySelector<HTMLElement>('[role="listbox"]:not([hidden])');
}

afterEach(() => {
  while (mountedRoots.length) {
    const entry = mountedRoots.pop()!;
    act(() => {
      entry.root.unmount();
    });
    entry.container.remove();
  }
  closeAllPopovers();
  document.body.innerHTML = "";
});

describe("StudioSelect (@ui-sdk/react/select)", () => {
  it("keeps legacy settings native but installs the themed Studio combobox through context", () => {
    const props = {
      value: "tls",
      onValueChange: vi.fn(),
      children: h("option", { value: "tls" }, "直接加密"),
    };
    const native = renderToStaticMarkup(h(Choice, props));
    expect(native).toContain("<select");
    expect(native).not.toContain('role="combobox"');
    const studio = renderToStaticMarkup(
      h(ChoiceRenderer.Provider, { value: StudioSelect }, h(Choice, props)),
    );
    expect(studio).toContain('role="combobox"');
    expect(studio).toContain('class="studio-picker');
    expect(studio).toContain("直接加密");
    expect(studio).toContain('data-tauri-drag-region="false"');
  });

  it("supports valid empty-string wire value natively and skips disabled options", async () => {
    const change = vi.fn();
    function Harness() {
      const [val, setVal] = useState("work");
      return h(StudioSelect, {
        "aria-label": "账户筛选",
        value: val,
        onValueChange: (next) => {
          change(next);
          setVal(next);
        },
        children: [
          h("option", { key: "all", value: "" }, "全部账户"),
          h(Fragment, { key: "accounts" }, h("option", { value: "work" }, "工作邮箱")),
          h("option", { key: "disabled", value: "plugin", disabled: true }, "尚不可用"),
        ],
      });
    }

    const { container } = mount(h(Harness));
    const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    expect(trigger.textContent).toContain("工作邮箱");

    act(() => {
      trigger.click();
    });
    await flushTimers();

    const listbox = openListbox()!;
    expect(listbox).not.toBeNull();
    expect(listbox.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(listbox.hasAttribute("data-shortcuts-off")).toBe(true);

    const options = [...listbox.querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options.map((el) => el.textContent?.replace("✓", "").trim())).toEqual([
      "全部账户",
      "工作邮箱",
      "尚不可用",
    ]);
    expect(options[1].getAttribute("aria-selected")).toBe("true");
    expect(options[1].hasAttribute("data-selected")).toBe(true);
    expect(options[2].getAttribute("aria-disabled")).toBe("true");

    // Clicking disabled option does nothing.
    act(() => {
      options[2].click();
    });
    await flushTimers();
    expect(change).not.toHaveBeenCalled();
    expect(openListbox()).not.toBeNull();

    // Selecting empty-string option works natively without sentinel prefix.
    act(() => {
      options[0].click();
    });
    await flushTimers();
    expect(change).toHaveBeenCalledWith("");
    expect(trigger.textContent).toContain("全部账户");
    expect(trigger.hasAttribute("data-placeholder")).toBe(false);
    expect(openListbox()).toBeNull();
  });

  it("maps unmatched values to null placeholder and blocks disabled empty-string option", async () => {
    const change = vi.fn();
    const { container } = mount(
      h(StudioSelect, {
        "aria-label": "分组",
        value: "missing-id",
        onValueChange: change,
        children: [
          h("option", { key: "empty", value: "", disabled: true }, "请选择分组"),
          h("option", { key: "work", value: "work" }, "工作"),
        ],
      }),
    );

    const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    expect(trigger.hasAttribute("data-placeholder")).toBe(true);

    act(() => {
      trigger.click();
    });
    await flushTimers();

    const options = [...openListbox()!.querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options[0].getAttribute("aria-disabled")).toBe("true");
    expect(options[0].getAttribute("aria-selected")).toBe("false");
    expect(options[1].getAttribute("aria-selected")).toBe("false");

    // Disabled empty option cannot be selected.
    act(() => {
      options[0].click();
    });
    await flushTimers();
    expect(change).not.toHaveBeenCalled();

    // Enabled option can be selected.
    act(() => {
      options[1].click();
    });
    await flushTimers();
    expect(change).toHaveBeenCalledWith("work");
  });

  it("does not open or change settings through a disabled picker", async () => {
    const change = vi.fn();
    const props: ChoiceProps = {
      value: "implicit",
      disabled: true,
      "aria-label": "连接安全",
      onValueChange: change,
      children: [
        h("option", { key: "tls", value: "implicit" }, "TLS"),
        h("option", { key: "starttls", value: "starttls" }, "STARTTLS"),
      ],
    };
    const html = renderToStaticMarkup(h(StudioSelect, props));
    expect(html).toContain('disabled=""');
    expect(html).toContain('aria-label="连接安全"');

    const { container } = mount(h(StudioSelect, props));
    const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    expect(trigger.disabled).toBe(true);
    expect(trigger.getAttribute("aria-disabled")).toBe("true");
    expect(trigger.hasAttribute("data-disabled")).toBe(true);

    act(() => {
      trigger.click();
    });
    await flushTimers();
    expect(openListbox()).toBeNull();
    expect(change).not.toHaveBeenCalled();
  });

  it("keeps chosen identity when asynchronous refresh reorders options", async () => {
    const change = vi.fn();
    const render = (ids: string[], value: string) =>
      h(StudioSelect, {
        "aria-label": "账户",
        value,
        onValueChange: change,
        children: ids.map((id) => h("option", { key: id, value: id }, id)),
      });

    const view = mount(render(["work", "personal"], "work"));
    const trigger = view.container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    expect(trigger.textContent).toContain("work");

    view.rerender(render(["personal", "work"], "work"));
    expect(trigger.textContent).toContain("work");

    act(() => {
      trigger.click();
    });
    await flushTimers();
    const options = [...openListbox()!.querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options[0].getAttribute("aria-selected")).toBe("false");
    expect(options[1].getAttribute("aria-selected")).toBe("true");

    act(() => {
      options[0].click();
    });
    await flushTimers();
    expect(change).toHaveBeenCalledWith("personal");
  });

  it("retains focus on trigger and prevents duplicate submission while async save disables picker (including on failure)", async () => {
    const submissions: string[] = [];
    let finishSave!: (ok: boolean) => void;

    function AsyncHarness() {
      const [value, setValue] = useState("a");
      const [saving, setSaving] = useState(false);
      return h(StudioSelect, {
        "aria-label": "语言",
        value,
        disabled: saving,
        onValueChange: (next) => {
          submissions.push(next);
          setSaving(true);
          finishSave = (ok: boolean) => {
            if (ok) setValue(next);
            setSaving(false);
          };
        },
        children: [
          h("option", { key: "a", value: "a" }, "中文"),
          h("option", { key: "b", value: "b" }, "English"),
        ],
      });
    }

    const { container } = mount(h(AsyncHarness));
    const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    act(() => {
      trigger.focus();
    });
    keydown(trigger, "ArrowDown");
    await flushTimers();

    const options = [...openListbox()!.querySelectorAll<HTMLElement>('[role="option"]')];
    act(() => {
      options[1].click();
    });
    await flushTimers();

    // Save is in flight: trigger stays focused (no native disabled attr), but aria-disabled/data-disabled are set and popup cannot reopen.
    expect(submissions).toEqual(["b"]);
    expect(trigger.disabled).toBe(false);
    expect(trigger.getAttribute("aria-disabled")).toBe("true");
    expect(trigger.hasAttribute("data-disabled")).toBe(true);
    expect(document.activeElement).toBe(trigger);
    expect(openListbox()).toBeNull();

    // Attempt to reopen or submit again while pending is blocked.
    act(() => {
      trigger.click();
    });
    keydown(trigger, "ArrowDown");
    await flushTimers();
    expect(openListbox()).toBeNull();
    expect(submissions).toEqual(["b"]);

    // Simulate async failure: value reverts/stays "a", disabled clears, picker becomes interactive again.
    act(() => {
      finishSave(false);
    });
    await flushTimers();
    expect(trigger.textContent).toContain("中文");
    expect(trigger.getAttribute("aria-disabled")).toBeNull();
    expect(trigger.hasAttribute("data-disabled")).toBe(false);

    // Can open and submit again after failure settles.
    act(() => {
      trigger.click();
    });
    await flushTimers();
    const retryOptions = [...openListbox()!.querySelectorAll<HTMLElement>('[role="option"]')];
    act(() => {
      retryOptions[1].click();
    });
    await flushTimers();
    expect(submissions).toEqual(["b", "b"]);
    act(() => {
      finishSave(true);
    });
    await flushTimers();
    expect(trigger.textContent).toContain("English");
  });

  it("unmounts cleanly while open without leaking listeners or throwing", async () => {
    const view = mount(
      h(StudioSelect, {
        "aria-label": "临时选择器",
        value: "one",
        onValueChange: vi.fn(),
        children: [
          h("option", { key: "one", value: "one" }, "One"),
          h("option", { key: "two", value: "two" }, "Two"),
        ],
      }),
    );
    const trigger = view.container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    act(() => {
      trigger.click();
    });
    await flushTimers();
    expect(openListbox()).not.toBeNull();

    view.unmount();
    await flushTimers();
    expect(openListbox()).toBeNull();
    expect(() => {
      window.dispatchEvent(new Event("pagehide"));
      window.dispatchEvent(new Event("blur"));
      closeAllPopovers();
    }).not.toThrow();
  });

  it("preserves sender validation and ARIA labels on both themed trigger and legacy select", () => {
    const props: ChoiceProps = {
      id: "compose-from",
      "aria-label": "发件人",
      "aria-invalid": true,
      "aria-describedby": "sender-error",
      value: "work",
      onValueChange: vi.fn(),
      children: h("option", { value: "work" }, "工作邮箱"),
    };
    for (const view of [
      h(Choice, props),
      h(ChoiceRenderer.Provider, { value: StudioSelect }, h(Choice, props)),
    ]) {
      const html = renderToStaticMarkup(view);
      expect(html).toContain('id="compose-from"');
      expect(html).toContain('aria-invalid="true"');
      expect(html).toContain('aria-describedby="sender-error"');
      expect(html).toContain('aria-label="发件人"');
    }
  });

  it("dismisses on pagehide, hidden visibilitychange, and closeAllPopovers without double-firing on window blur", async () => {
    const { container } = mount(
      h(StudioSelect, {
        "aria-label": "窗口关闭测试",
        value: "a",
        onValueChange: vi.fn(),
        children: [
          h("option", { key: "a", value: "a" }, "A"),
          h("option", { key: "b", value: "b" }, "B"),
        ],
      }),
    );
    const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;

    for (const fire of [
      () => window.dispatchEvent(new Event("pagehide")),
      () => {
        Object.defineProperty(document, "hidden", { value: true, configurable: true });
        document.dispatchEvent(new Event("visibilitychange"));
        Object.defineProperty(document, "hidden", { value: false, configurable: true });
      },
      () => closeAllPopovers(),
      () => window.dispatchEvent(new Event("blur")),
    ]) {
      act(() => {
        trigger.click();
      });
      await flushTimers();
      expect(openListbox()).not.toBeNull();

      act(() => {
        fire();
      });
      await flushTimers();
      expect(openListbox()).toBeNull();
    }
  });

  it("allocates monotonic Safari 16 fallback inline zIndex when supportsTopLayer() is false", async () => {
    // jsdom lacks showPopover(), exercising the !supportsTopLayer() Safari 16 fallback path.
    const baseLayer = acquireFallbackLayer();
    try {
      const { container } = mount(
        h(StudioSelect, {
          "aria-label": "层级测试",
          value: "a",
          onValueChange: vi.fn(),
          children: [
            h("option", { key: "a", value: "a" }, "A"),
            h("option", { key: "b", value: "b" }, "B"),
          ],
        }),
      );
      const trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!;
      act(() => {
        trigger.click();
      });
      await flushTimers();

      const listbox = openListbox()!;
      expect(listbox).not.toBeNull();
      const z = Number(listbox.style.zIndex);
      expect(Number.isFinite(z)).toBe(true);
      expect(z).toBeGreaterThan(baseLayer.content);
      // Must use our monotonic allocator, not @ui-sdk's hardcoded 2147483000 fallback.
      expect(z).toBeLessThan(10000);

      // A nested layer acquired while Select is open stacks strictly above Select's zIndex.
      const nestedLayer = acquireFallbackLayer();
      try {
        expect(nestedLayer.backdrop).toBeGreaterThan(z);
        expect(nestedLayer.content).toBeGreaterThan(nestedLayer.backdrop);
      } finally {
        nestedLayer.release();
      }
    } finally {
      baseLayer.release();
    }
  });

  it("allows native selects only at Choice fallback, never in Studio screens", () => {
    const sources = import.meta.glob<string>("../src/**/*.tsx", {
      eager: true,
      query: "?raw",
      import: "default",
    });
    const native: string[] = [];
    for (const [path, source] of Object.entries(sources)) {
      const file = ts.createSourceFile(
        path,
        source,
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TSX,
      );
      const visit = (node: ts.Node) => {
        if (
          (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) &&
          node.tagName.getText(file) === "select"
        ) {
          native.push(path);
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }
    expect(native).toEqual(["../src/settings/Choice.tsx"]);
  });

  it("does not run workspace shortcuts when focused inside a picker or its portaled options", () => {
    for (const role of ["combobox", "listbox", "option"]) {
      const target = {
        closest: (selector: string) =>
          selector.includes(`[role="${role}"]`) ? {} : null,
      };
      expect(isShortcutBlockedTarget(target)).toBe(true);
    }
    expect(isShortcutBlockedTarget({ closest: () => null })).toBe(false);
  });
});
