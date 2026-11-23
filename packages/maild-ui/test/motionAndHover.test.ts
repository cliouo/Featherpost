import * as ts from "typescript";
import { describe, expect, it } from "vitest";

const packagesDir = decodeURIComponent(new URL("../..", import.meta.url).pathname).replace(/\/$/, "");

function collectFiles(root: string, ext: string): Record<string, string> {
  const out: Record<string, string> = {};
  const files = ts.sys.readDirectory(root, [ext], ["node_modules", "dist"]);
  for (const full of files) {
    const content = ts.sys.readFile(full, "utf8");
    if (content === undefined) continue;
    const normalized = full.replaceAll("\\", "/");
    const rel = normalized.startsWith(`${packagesDir}/`)
      ? normalized.slice(packagesDir.length + 1)
      : normalized;
    out[rel] = content;
  }
  return out;
}

const stylesheets = {
  ...collectFiles(`${packagesDir}/maild-ui/src`, ".css"),
  ...collectFiles(`${packagesDir}/featherpost-studio/src`, ".css"),
};

const tsxSources = {
  ...collectFiles(`${packagesDir}/maild-ui/src`, ".tsx"),
  ...collectFiles(`${packagesDir}/featherpost-studio/src`, ".tsx"),
};

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
  const tokens = stylesheets["maild-ui/src/styles/tokens.css"]!;

  it("defines shared short motion durations (90-200ms) and easing curves in tokens.css", () => {
    expect(Object.keys(stylesheets).length).toBeGreaterThanOrEqual(11);
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
});

describe("shared hover states (#27)", () => {
  const tokens = stylesheets["maild-ui/src/styles/tokens.css"]!;
  const studio = stylesheets["featherpost-studio/src/studio.css"]!;

  it("defines the full hover token set for light and dark themes in both maild-ui and Studio", () => {
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

  it("preserves :focus-visible styles in both maild-ui and Studio", () => {
    expect(stylesheets["maild-ui/src/styles/app.css"]).toContain(":focus-visible");
    expect(studio).toContain(":focus-visible");
  });

  it("keeps shared settings form CSS synchronized between maild-ui and Studio", () => {
    expect(stylesheets["featherpost-studio/src/settings/forms.css"]).toBe(
      stylesheets["maild-ui/src/settings/settings.css"],
    );
  });

  it("provides title tooltips on icon-only buttons across maild-ui and Studio", () => {
    const missing: string[] = [];
    const iconClasses = ["icon-button", "folder-disclosure", "message-star"];
    for (const [filePath, source] of Object.entries(tsxSources)) {
      const file = ts.createSourceFile(filePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: ts.Node) => {
        if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
          const tag = node.tagName.getText(file);
          if (tag === "button" || tag === "summary") {
            const attrs = node.attributes.properties.filter(ts.isJsxAttribute);
            const attrText = (name: string) =>
              attrs.find((a) => a.name.getText(file) === name)?.initializer?.getText(file) ?? "";
            const cls = attrText("className");
            const hasTitle = attrs.some((a) => a.name.getText(file) === "title");
            if (iconClasses.some((c) => cls.includes(c)) && !hasTitle) {
              const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
              missing.push(`${filePath}:${line + 1} (${tag} className=${cls})`);
            }
          } else if (tag === "PopoverMenu") {
            const attrs = node.attributes.properties.filter(ts.isJsxAttribute);
            const attrText = (name: string) =>
              attrs.find((a) => a.name.getText(file) === name)?.initializer?.getText(file) ?? "";
            const cls = attrText("triggerClassName");
            const hasTitle = attrs.some((a) => a.name.getText(file) === "triggerTitle");
            if (iconClasses.some((c) => cls.includes(c)) && !hasTitle) {
              const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
              missing.push(`${filePath}:${line + 1} (${tag} triggerClassName=${cls})`);
            }
          }
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }
    expect(missing).toEqual([]);
  });
});
