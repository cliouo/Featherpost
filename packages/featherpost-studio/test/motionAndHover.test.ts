import * as ts from "typescript";
import { describe, expect, it } from "vitest";

const srcDir = decodeURIComponent(new URL("../src", import.meta.url).pathname).replace(/\/$/, "");

function collectFiles(root: string, ext: string): Record<string, string> {
  const out: Record<string, string> = {};
  const files = ts.sys.readDirectory(root, [ext], ["node_modules", "dist"]);
  for (const full of files) {
    const content = ts.sys.readFile(full, "utf8");
    if (content === undefined) continue;
    const normalized = full.replaceAll("\\", "/");
    const rel = normalized.startsWith(`${srcDir}/`)
      ? normalized.slice(srcDir.length + 1)
      : normalized;
    out[rel] = content;
  }
  return out;
}

const stylesheets = collectFiles(srcDir, ".css");
const tsxSources = collectFiles(srcDir, ".tsx");

const REQUIRED_HOVER_TOKENS = [
  "--hover-bg",
  "--hover-bg-strong",
  "--hover-surface",
  "--hover-row",
  "--hover-selected",
  "--hover-checked",
  "--hover-border",
  "--hover-accent",
  "--hover-danger-bg",
  "--hover-danger-border",
  "--hover-danger-ink",
  "--active-bg",
] as const;

describe("shared motion tokens (#25)", () => {
  const tokens = stylesheets["styles/tokens.css"]!;

  it("defines shared short motion durations (90-200ms) and easing curves in tokens.css", () => {
    expect(Object.keys(stylesheets).length).toBeGreaterThanOrEqual(8);
    for (const name of ["--motion-instant", "--motion-fast", "--motion-base", "--motion-slow"]) {
      const match = new RegExp(`${name}:\\s*(\\d+)ms`).exec(tokens);
      expect(match, `missing ${name}`).not.toBeNull();
      const ms = Number(match![1]);
      expect(ms).toBeGreaterThanOrEqual(80);
      expect(ms).toBeLessThanOrEqual(200);
    }
    expect(tokens).toMatch(/--ease-standard:\s*cubic-bezier\(/);
    expect(tokens).toMatch(/--ease-exit:\s*cubic-bezier\(/);
  });

  it("disables or shortens transitions and animations globally under prefers-reduced-motion", () => {
    expect(tokens).toMatch(/@media\s*\(prefers-reduced-motion:\s*reduce\)/);
    expect(tokens).toContain("animation-duration: 0.01ms !important;");
    expect(tokens).toContain("transition-duration: 0.01ms !important;");
    expect(tokens).toContain("scroll-behavior: auto !important;");
  });

  it("never uses transition: all or hardcoded transition durations in any stylesheet", () => {
    const violations: string[] = [];
    for (const [file, css] of Object.entries(stylesheets)) {
      const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");
      if (/transition\s*:\s*all\b/i.test(stripped)) {
        violations.push(`${file}: uses transition: all`);
      }
      for (const match of stripped.matchAll(/transition\s*:([^;{}]+);/g)) {
        const value = match[1]!;
        if (/\bnone\b/.test(value)) continue;
        if (/(?<!var\([^)]*)\b\d+(?:\.\d+)?m?s\b/.test(value)) {
          violations.push(`${file}: hardcoded transition duration -> ${value.trim()}`);
        }
      }
    }
    expect(violations).toEqual([]);
  });

  it("keeps all stylesheets compatible with WKWebView / Safari 15", () => {
    const incompatible: string[] = [];
    for (const [file, css] of Object.entries(stylesheets)) {
      const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");
      for (const pattern of ["color-mix(", "@starting-style", "transition-behavior"]) {
        if (stripped.includes(pattern)) {
          incompatible.push(`${file}: contains ${pattern}`);
        }
      }
    }
    expect(incompatible).toEqual([]);
  });

  it("uses @ui-sdk data-attribute transitions and portal-safe Settings variables without legacy dialog-positioner or Radix variables", () => {
    const studio = stylesheets["studio.css"]!;
    const contextmenu = stylesheets["contextmenu.css"]!;
    const tooltip = stylesheets["tooltip.css"]!;
    const settings = stylesheets["settings/settings.css"]!;
    const forms = stylesheets["settings/forms.css"]!;

    expect(studio).not.toContain(".dialog-positioner");
    expect(studio).not.toContain("--radix-dropdown-menu-");
    expect(studio).toContain("--available-height");
    expect(studio).toContain("--transform-origin");
    expect(studio).toContain("[data-starting-style]");
    expect(studio).toContain("[data-ending-style]");
    expect(contextmenu).not.toContain("--radix-");
    expect(contextmenu).not.toContain("data-state");
    expect(contextmenu).toContain("--available-height");
    expect(contextmenu).toContain("--available-width");
    expect(contextmenu).toContain("--transform-origin");
    expect(contextmenu).toContain("[data-open]");
    expect(contextmenu).toContain("[data-starting-style]");
    expect(contextmenu).toContain("[data-ending-style]");
    expect(tooltip).toContain("--available-width");
    expect(tooltip).toContain("--transform-origin");
    expect(tooltip).toContain("[data-starting-style]");
    expect(tooltip).toContain("[data-ending-style]");
    expect(tooltip).toContain("[data-instant]");
    expect(tooltip.replace(/\/\*[\s\S]*?\*\//g, "")).not.toMatch(/overflow\s*:\s*auto/);
    expect(settings).toContain("body[data-studio-settings],\n.studio-settings");
    expect(forms).toContain("body[data-studio-settings] .mail-dialog fieldset");
  });
});

describe("shared hover states (#27)", () => {
  const tokens = stylesheets["styles/tokens.css"]!;
  const studio = stylesheets["studio.css"]!;

  it("defines the full hover token set for light and dark themes in tokens.css and Studio", () => {
    for (const token of REQUIRED_HOVER_TOKENS) {
      const tokenMatches = tokens.match(new RegExp(`${token}:`, "g")) ?? [];
      expect(tokenMatches.length, `tokens.css ${token} light+dark`).toBeGreaterThanOrEqual(3);
      const studioMatches = studio.match(new RegExp(`${token}:`, "g")) ?? [];
      expect(studioMatches.length, `studio.css ${token} light+dark`).toBeGreaterThanOrEqual(2);
    }
  });

  it("replaces brightness filter hover hack in Studio with tokenized hover treatments", () => {
    expect(studio).not.toMatch(/filter:\s*brightness\(/);
    expect(studio).toContain("--hover-danger-bg");
    expect(studio).toContain("--hover-danger-ink");
  });

  it("preserves :focus-visible styles in Studio", () => {
    expect(studio).toContain(":focus-visible");
  });

  it("requires direct StudioTooltip wrapping on icon-only and menu/popover triggers and forbids native title double-tooltips", () => {
    /**
     * Documented exceptions that intentionally retain native `title`:
     * - `ui/TokenInput.tsx`: high-volume chip items and chip remove buttons
     * - Truncated text / long-list items (`Workspace.tsx` chip body / row-account,
     *   `Shell.tsx` folder-link, `MailboxNavigation.tsx` account-heading,
     *   `Activity.tsx` rows, `compose/module.tsx` draft timestamps, `ReadingPane.tsx` iframe)
     */
    const NATIVE_TITLE_ICON_EXCEPTIONS = new Set(["ui/TokenInput.tsx"]);

    const iconClasses = [
      "icon-button",
      "folder-disclosure",
      "message-star",
      "search-filter-trigger",
      "search-filter-chip-remove",
    ];
    const triggerTags = new Set(["button", "summary", "StudioPopoverClose"]);
    const missing: string[] = [];
    const doubleTitles: string[] = [];

    const getDirectTooltipParent = (
      jsxNode: ts.JsxElement | ts.JsxSelfClosingElement,
      file: ts.SourceFile,
    ): ts.JsxElement | null => {
      const parent = jsxNode.parent;
      if (!ts.isJsxElement(parent)) return null;
      if (parent.openingElement.tagName.getText(file) !== "StudioTooltip") {
        return null;
      }
      // Verify this node is the direct JSX element child of <StudioTooltip> (ignoring whitespace text).
      const elementChildren = parent.children.filter(
        (child) =>
          ts.isJsxElement(child) ||
          ts.isJsxSelfClosingElement(child) ||
          ts.isJsxExpression(child),
      );
      if (elementChildren.length !== 1 || elementChildren[0] !== jsxNode) {
        return null;
      }
      return parent;
    };

    const hasAttr = (
      opening: ts.JsxOpeningLikeElement,
      file: ts.SourceFile,
      name: string,
    ): boolean =>
      opening.attributes.properties.some(
        (prop) => ts.isJsxAttribute(prop) && prop.name.getText(file) === name,
      );

    const attrText = (
      opening: ts.JsxOpeningLikeElement,
      file: ts.SourceFile,
      name: string,
    ): string => {
      const attr = opening.attributes.properties.find(
        (prop): prop is ts.JsxAttribute =>
          ts.isJsxAttribute(prop) && prop.name.getText(file) === name,
      );
      return attr?.initializer?.getText(file) ?? "";
    };

    for (const [filePath, source] of Object.entries(tsxSources)) {
      const file = ts.createSourceFile(
        filePath,
        source,
        ts.ScriptTarget.Latest,
        true,
        ts.ScriptKind.TSX,
      );

      const visit = (node: ts.Node) => {
        // 1. Statically forbid native `title=` on ANY direct <StudioTooltip> trigger child!
        if (
          ts.isJsxElement(node) &&
          node.openingElement.tagName.getText(file) === "StudioTooltip"
        ) {
          for (const child of node.children) {
            const childOpening = ts.isJsxElement(child)
              ? child.openingElement
              : ts.isJsxSelfClosingElement(child)
                ? child
                : null;
            if (childOpening && hasAttr(childOpening, file, "title")) {
              const { line } = file.getLineAndCharacterOfPosition(
                childOpening.getStart(file),
              );
              doubleTitles.push(
                `${filePath}:${line + 1} (<${childOpening.tagName.getText(file)}> inside <StudioTooltip> must not also set native title=)`,
              );
            }
          }
        }

        // 2. Check icon-only buttons, summary, StudioPopoverClose, and Accounts.tsx AccountMore trigger.
        const jsxElem = ts.isJsxElement(node)
          ? node
          : ts.isJsxSelfClosingElement(node)
            ? node
            : null;
        if (jsxElem) {
          const opening = ts.isJsxElement(jsxElem)
            ? jsxElem.openingElement
            : jsxElem;
          const tag = opening.tagName.getText(file);

          if (triggerTags.has(tag) && !NATIVE_TITLE_ICON_EXCEPTIONS.has(filePath)) {
            const cls = attrText(opening, file, "className");
            const isIconControl =
              tag === "StudioPopoverClose" ||
              iconClasses.some((c) => cls.includes(c));
            const isAccountsMoreTrigger =
              filePath === "settings/Accounts.tsx" &&
              tag === "button" &&
              attrText(opening, file, "aria-label") === "{label}";

            if (isIconControl || isAccountsMoreTrigger) {
              const directTooltip = getDirectTooltipParent(jsxElem, file);
              const hasLabelProp =
                directTooltip !== null &&
                hasAttr(directTooltip.openingElement, file, "label");
              if (!hasLabelProp) {
                const { line } = file.getLineAndCharacterOfPosition(
                  opening.getStart(file),
                );
                missing.push(
                  `${filePath}:${line + 1} (${tag} className=${cls || '""'}) must be directly wrapped in <StudioTooltip label={...}>`,
                );
              }
            }
          }
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }

    expect(missing).toEqual([]);
    expect(doubleTitles).toEqual([]);
  });
});
