import * as ts from "typescript";
import { describe, expect, it } from "vitest";

function resolveFromTest(relative: string): string {
  const pathname = decodeURIComponent(new URL(relative, import.meta.url).pathname);
  return /^\/[A-Za-z]:\//.test(pathname) ? pathname.slice(1) : pathname;
}

function readSource(relative: string): string {
  const path = resolveFromTest(relative);
  const text = ts.sys.readFile(path);
  if (text === undefined) throw new Error(`Missing source file: ${path}`);
  return text;
}

function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

function ruleBody(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const matches = [
    ...stripComments(css).matchAll(new RegExp(`(?:^|[}\\s])${escaped}\\s*\\{([^}]*)\\}`, "gm")),
  ];
  return matches.map((m) => m[1]).join("\n");
}

const scrollbarsCss = readSource("../src/styles/scrollbars.css");
const tokensCss = readSource("../src/styles/tokens.css");
const composeCss = readSource("../src/compose/compose.css");
const studioMain = readSource("../src/main.tsx");
const studioCss = readSource("../src/studio.css");
const studioNavigationCss = readSource("../src/navigation.css");
const studioActivityCss = readSource("../src/activity.css");
const studioFormsCss = readSource("../src/settings/forms.css");
const studioCommandPaletteCss = readSource("../src/command-palette.css");

describe("Shared scrollbar styling and layout stability (#29)", () => {
  it("loads the shared scrollbars.css stylesheet in Studio entry", () => {
    expect(studioMain).toMatch(/import\s+["']\.\/styles\/scrollbars\.css["']/);
  });

  it("defines lightweight WebKit scrollbar styles with expanded hit area, hover/active states, and zero Radix selectors", () => {
    const clean = stripComments(scrollbarsCss);
    expect(clean).toContain(":where(*)::-webkit-scrollbar");
    expect(clean).not.toContain("radix");
    expect(clean).toContain("::-webkit-scrollbar-track");
    expect(clean).toContain("::-webkit-scrollbar-thumb");
    expect(clean).toContain("::-webkit-scrollbar-thumb:vertical");
    expect(clean).toContain("::-webkit-scrollbar-thumb:horizontal");
    expect(clean).toContain("::-webkit-scrollbar-thumb:hover");
    expect(clean).toContain("::-webkit-scrollbar-thumb:active");
    expect(clean).toContain("border-radius: 999px");
    expect(clean).toContain("background-clip: content-box");
    expect(clean).toContain("var(--scrollbar-track, transparent)");

    // Standard scrollbar-width / scrollbar-color must only appear inside the
    // non-WebKit fallback so WebKit/Blink never disables ::-webkit-scrollbar.
    const outsideSupports = clean.replace(
      /@supports\s+not\s+selector\(::-webkit-scrollbar\)\s*\{[\s\S]*?\}\s*\}/g,
      "",
    );
    expect(outsideSupports).not.toMatch(/scrollbar-(?:width|color)\s*:/);
  });

  it("defines light and dark scrollbar tokens in both maild-ui and featherpost-studio", () => {
    for (const token of [
      "--scrollbar-size",
      "--scrollbar-track",
      "--scrollbar-thumb",
      "--scrollbar-thumb-hover",
      "--scrollbar-thumb-active",
    ]) {
      expect(tokensCss).toContain(token);
    }
    expect(tokensCss.match(/--scrollbar-thumb:/g)?.length).toBeGreaterThanOrEqual(3);

    const studioDark = ruleBody(studioCss, ':root[data-studio-theme="dark"]');
    expect(studioCss).toContain("--scrollbar-thumb:");
    expect(studioDark).toContain("--scrollbar-thumb:");
    expect(studioDark).toContain("--scrollbar-thumb-hover:");
    expect(studioDark).toContain("--scrollbar-thumb-active:");
  });

  it("replaces ambiguous overflow: auto across all frontend stylesheets", () => {
    const cssFiles = ts.sys.readDirectory(resolveFromTest("../src"), [".css"]);
    expect(cssFiles.length).toBeGreaterThanOrEqual(8);
    const offenders: string[] = [];
    for (const file of cssFiles) {
      const source = ts.sys.readFile(file) ?? "";
      if (/\boverflow\s*:\s*auto\b/.test(stripComments(source))) {
        offenders.push(file);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("reserves a stable vertical scrollbar gutter without horizontal overflow on dynamic scroll containers", () => {
    const containers: Array<[string, string, string]> = [
      // Featherpost Studio
      ["studio .settings-content", studioCss, ".settings-content"],
      ["studio .settings-nav", studioCss, ".settings-nav"],
      ["studio .sidebar-scroll", studioCss, ".sidebar-scroll"],
      ["studio .message-scroll", studioCss, ".message-scroll"],
      ["studio .letter-scroll", studioCss, ".letter-scroll"],
      ["studio .page-scroll", studioCss, ".page-scroll"],
      ["studio .mail-dialog", studioCss, ".mail-dialog"],
      ["studio .action-message-list", studioCss, ".action-message-list"],
      ["studio .sheet", studioCss, ".sheet"],
      ["studio .link-target", studioCss, ".link-target"],
      ["studio .directory-body", studioNavigationCss, ".directory-body"],
      ["studio .group-manager", studioNavigationCss, ".group-manager"],
      ["studio .activity-scroll", studioActivityCss, ".studio-activity .activity-scroll"],
      ["studio .command-palette-list", studioCommandPaletteCss, ".command-palette-list"],
      ["studio .settings-field textarea", studioFormsCss, ".settings-field textarea"],
      // Shared compose
      ["compose .compose-scroll", composeCss, ".compose-scroll"],
      ["compose textarea", composeCss, ".compose-body textarea"],
    ];

    for (const [label, css, selector] of containers) {
      const body = ruleBody(css, selector);
      expect(body, `${label} must set overflow-y: scroll`).toMatch(/overflow-y\s*:\s*scroll/);
      expect(body, `${label} must set overflow-x: hidden`).toMatch(/overflow-x\s*:\s*hidden/);
    }
  });
});
