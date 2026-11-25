import { createElement as h, Fragment, type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { StudioSelect } from "../src/StudioSelect";
import { Choice, ChoiceRenderer, type ChoiceProps } from "../src/settings/Choice";
import { isShortcutBlockedTarget } from "../src/Workspace";
import * as ts from "typescript";

function picker(props: ChoiceProps) {
  let root: ReactElement;
  function Probe() { root = StudioSelect(props); return root; }
  const html = renderToStaticMarkup(h(Probe));
  return { html, root: root! as ReactElement<{ value: string; onValueChange(value: string): void }> };
}

it("keeps legacy settings native but installs the themed Studio combobox through context", () => {
  const props = { value: "tls", onValueChange: vi.fn(), children: h("option", { value: "tls" }, "直接加密") };
  const native = renderToStaticMarkup(h(Choice, props));
  expect(native).toContain('<select');
  expect(native).not.toContain('role="combobox"');
  const studio = renderToStaticMarkup(h(ChoiceRenderer.Provider, { value: StudioSelect }, h(Choice, props)));
  expect(studio).toContain('role="combobox"');
  expect(studio).toContain('class="studio-picker');
  expect(studio).toContain('直接加密');
});

it("maps the empty account filter and provider wire values without a sentinel collision", () => {
  const change = vi.fn();
  const { root } = picker({ value: "work", onValueChange: change, children: [
    h("option", { key: "all", value: "" }, "全部账户"),
    h(Fragment, { key: "accounts" }, h("option", { value: "work" }, "工作邮箱")),
    h("option", { key: "disabled", value: "plugin", disabled: true }, "尚不可用"),
  ] });
  expect(root.props.value).toBe("choice:work");
  root.props.onValueChange("choice:");
  root.props.onValueChange("choice:work");
  root.props.onValueChange("choice:plugin");
  root.props.onValueChange("constructor");
  expect(change.mock.calls).toEqual([[""], ["work"]]);
});

it("does not change settings through a disabled picker", () => {
  const change = vi.fn();
  const { root, html } = picker({ value: "implicit", disabled: true, "aria-label": "连接安全", onValueChange: change,
    children: [h("option", { key: "tls", value: "implicit" }, "TLS"), h("option", { key: "starttls", value: "starttls" }, "STARTTLS")] });
  expect(html).toContain('disabled=""');
  expect(html).toContain('aria-label="连接安全"');
  root.props.onValueChange("choice:starttls");
  expect(change).not.toHaveBeenCalled();
});

it("keeps the chosen account identity when asynchronous refresh reorders options", () => {
  const change = vi.fn();
  const account = (id: string) => h("option", { key: id, value: id }, id);
  const before = picker({ value: "work", onValueChange: change, children: [account("work"), account("personal")] });
  const after = picker({ value: "work", onValueChange: change, children: [account("personal"), account("work")] });
  expect(before.root.props.value).toBe(after.root.props.value);
  after.root.props.onValueChange(before.root.props.value);
  expect(change).toHaveBeenCalledWith("work");
});

it("preserves sender validation and labels on the themed trigger and legacy select", () => {
  const props: ChoiceProps = { id: "compose-from", "aria-label": "发件人", "aria-invalid": true,
    "aria-describedby": "sender-error", value: "work", onValueChange: vi.fn(), children: h("option", { value: "work" }, "工作邮箱") };
  for (const view of [h(Choice, props), h(ChoiceRenderer.Provider, { value: StudioSelect }, h(Choice, props))]) {
    const html = renderToStaticMarkup(view);
    expect(html).toContain('id="compose-from"');
    expect(html).toContain('aria-invalid="true"');
    expect(html).toContain('aria-describedby="sender-error"');
    expect(html).toContain('aria-label="发件人"');
  }
});

it("allows native selects only at Choice fallback, never in Studio screens", () => {
  // This checks every source screen, not a hand-picked list: shared settings,
  // composer, move dialogs and future modules cannot silently bypass theming.
  const sources = import.meta.glob<string>("../src/**/*.tsx", {
    eager: true, query: "?raw", import: "default",
  });
  const native: string[] = [];
  for (const [path, source] of Object.entries(sources)) {
    const file = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const visit = (node: ts.Node) => {
      if ((ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) && node.tagName.getText(file) === "select") native.push(path);
      ts.forEachChild(node, visit);
    };
    visit(file);
  }
  expect(native).toEqual(["../src/settings/Choice.tsx"]);
});

it("does not run workspace shortcuts when focused inside a picker or its portaled options", () => {
  for (const role of ["combobox", "listbox", "option"]) {
    const target = { closest: (selector: string) => selector.includes(`[role="${role}"]`) ? {} : null };
    expect(isShortcutBlockedTarget(target)).toBe(true);
  }
  expect(isShortcutBlockedTarget({ closest: () => null })).toBe(false);
});
