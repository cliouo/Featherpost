import { createElement as h, Fragment, type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { StudioSelect } from "../../featherpost-studio/src/StudioSelect";
import { Choice, ChoiceRenderer, type ChoiceProps } from "../src/settings/Choice";
import { shortcutFor } from "../src/mailbox/shortcuts";

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
  expect(root.props.value).toBe("option-1");
  root.props.onValueChange("option-0");
  root.props.onValueChange("option-1");
  root.props.onValueChange("option-2");
  root.props.onValueChange("constructor");
  expect(change.mock.calls).toEqual([[""], ["work"]]);
});

it("does not change settings through a disabled picker", () => {
  const change = vi.fn();
  const { root, html } = picker({ value: "implicit", disabled: true, "aria-label": "连接安全", onValueChange: change,
    children: [h("option", { key: "tls", value: "implicit" }, "TLS"), h("option", { key: "starttls", value: "starttls" }, "STARTTLS")] });
  expect(html).toContain('disabled=""');
  expect(html).toContain('aria-label="连接安全"');
  root.props.onValueChange("option-1");
  expect(change).not.toHaveBeenCalled();
});

it("does not run mail actions when typing into a picker or its portaled options", () => {
  for (const role of ["combobox", "listbox", "option"]) {
    const target = { tagName: "SPAN", closest: (selector: string) => selector.includes(`[role="${role}"]`) ? {} : null };
    for (const key of ["r", "e", "j", "k", "s"]) expect(shortcutFor({ key }, target, false)).toBeNull();
  }
});
