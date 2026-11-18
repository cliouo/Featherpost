import { createElement, type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { ErrorBoundary } from "../src/app/ErrorBoundary";

describe("ErrorBoundary：渲染异常兜底与恢复", () => {
  it("正常子树原样渲染", () => {
    const html = renderToStaticMarkup(
      createElement(ErrorBoundary, null, createElement("p", null, "收件箱正常")),
    );
    expect(html).toBe("<p>收件箱正常</p>");
  });

  it("捕获渲染错误后展示中文恢复卡片，点击「重新载入界面」或切换 resetKey 可恢复", () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const onReset = vi.fn();
    try {
      const boundary = new ErrorBoundary({
        resetKey: "view-a",
        onReset,
        children: createElement("p", null, "恢复后的内容"),
      });
      // 模拟 React 在子组件抛错时驱动 getDerivedStateFromError + componentDidCatch
      const patch = ErrorBoundary.getDerivedStateFromError();
      boundary.state = { ...boundary.state, ...patch };
      boundary.componentDidCatch(new Error("boom"), { componentStack: "\n  at BrokenView" });
      expect(consoleSpy).toHaveBeenCalledOnce();

      const fallbackNode = boundary.render() as ReactElement;
      const fallbackHtml = renderToStaticMarkup(fallbackNode);
      expect(fallbackHtml).toContain('role="alert"');
      expect(fallbackHtml).toContain("界面遇到意外问题");
      expect(fallbackHtml).toContain("重新载入界面");

      // 点击恢复按钮：通过 setState 清空错误态并回调 onReset
      let nextState = boundary.state;
      boundary.setState = ((updater: unknown) => {
        nextState = {
          ...nextState,
          ...(typeof updater === "function" ? updater(nextState) : (updater as object)),
        };
        boundary.state = nextState;
      }) as typeof boundary.setState;

      boundary.reset();
      expect(boundary.state.failed).toBe(false);
      expect(onReset).toHaveBeenCalledOnce();
      expect(renderToStaticMarkup(boundary.render() as ReactElement)).toBe("<p>恢复后的内容</p>");

      // 再次进入错误态后切换视图（resetKey 变化）自动恢复
      boundary.state = ErrorBoundary.getDerivedStateFromError();
      expect(boundary.state.failed).toBe(true);
      const prevProps = boundary.props;
      (boundary as { props: typeof prevProps }).props = { ...prevProps, resetKey: "view-b" };
      boundary.componentDidUpdate(prevProps);
      expect(boundary.state.failed).toBe(false);
    } finally {
      consoleSpy.mockRestore();
    }
  });
});
