import * as ts from "typescript";
import { describe, expect, it } from "vitest";
import studioPkg from "../package.json";

const srcDir = decodeURIComponent(
  new URL("../src", import.meta.url).pathname,
).replace(/\/$/, "");

function collectSources(): Array<{ rel: string; source: string; file: ts.SourceFile }> {
  const files = ts.sys.readDirectory(srcDir, [".ts", ".tsx"], ["node_modules", "dist"]);
  return files.flatMap((full) => {
    const source = ts.sys.readFile(full, "utf8");
    if (source === undefined) return [];
    const normalized = full.replaceAll("\\", "/");
    const rel = normalized.startsWith(`${srcDir}/`)
      ? normalized.slice(srcDir.length + 1)
      : normalized;
    const kind = rel.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
    return [
      {
        rel,
        source,
        file: ts.createSourceFile(rel, source, ts.ScriptTarget.Latest, true, kind),
      },
    ];
  });
}

function isAllowedUiSdkImporter(rel: string): boolean {
  return rel.startsWith("ui/") || rel.startsWith("agent/");
}

function collectModuleSpecifiers(file: ts.SourceFile): Array<{ specifier: string; line: number }> {
  const found: Array<{ specifier: string; line: number }> = [];
  const record = (node: ts.Node, text: string) => {
    const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
    found.push({ specifier: text, line: line + 1 });
  };
  const visit = (node: ts.Node) => {
    // Static import / type-only import / namespace import: import ... from "..."
    if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)) {
      record(node, node.moduleSpecifier.text);
    }
    // Re-export: export ... from "..."
    if (
      ts.isExportDeclaration(node) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      record(node, node.moduleSpecifier.text);
    }
    // Import equals: import x = require("...")
    if (
      ts.isImportEqualsDeclaration(node) &&
      ts.isExternalModuleReference(node.moduleReference) &&
      ts.isStringLiteral(node.moduleReference.expression)
    ) {
      record(node, node.moduleReference.expression.text);
    }
    // Dynamic import("...") or require("...")
    if (ts.isCallExpression(node) && node.arguments.length >= 1) {
      const arg = node.arguments[0]!;
      const isDynamicImport = node.expression.kind === ts.SyntaxKind.ImportKeyword;
      const isRequire =
        ts.isIdentifier(node.expression) && node.expression.text === "require";
      if (isDynamicImport || isRequire) {
        if (ts.isStringLiteral(arg) || ts.isNoSubstitutionTemplateLiteral(arg)) {
          record(node, arg.text);
        } else if (ts.isTemplateExpression(arg)) {
          record(node, arg.getText(file));
        }
      }
    }
    // TypeScript import("@ui-sdk/...") type queries
    if (
      ts.isImportTypeNode(node) &&
      ts.isLiteralTypeNode(node.argument) &&
      ts.isStringLiteral(node.argument.literal)
    ) {
      record(node, node.argument.literal.text);
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return found;
}

describe("UI SDK architectural boundary", () => {
  const sources = collectSources();

  it("restricts all @ui-sdk/* imports, re-exports, and dynamic imports to src/ui/ (and src/agent/)", () => {
    expect(sources.length).toBeGreaterThan(20);
    const violations: string[] = [];
    for (const { rel, file } of sources) {
      for (const { specifier, line } of collectModuleSpecifiers(file)) {
        if (specifier.includes("@ui-sdk/core/web.css")) {
          violations.push(`${rel}:${line} imports forbidden stylesheet ${specifier}`);
          continue;
        }
        if (
          specifier.includes("@radix-ui/react-select") ||
          specifier.includes("@radix-ui/react-dialog") ||
          specifier.includes("@radix-ui/react-dropdown-menu")
        ) {
          violations.push(`${rel}:${line} still imports retired ${specifier}`);
          continue;
        }
        if (specifier.includes("@ui-sdk/") && !isAllowedUiSdkImporter(rel)) {
          violations.push(`${rel}:${line} imports ${specifier} outside src/ui/`);
        }
      }
    }
    expect(violations).toEqual([]);
  });

  it("permanently blocks WebMCP (registerTools / modelContext) anywhere in src/", () => {
    const violations: string[] = [];
    const forbidden = new Set(["registerTools", "modelContext"]);

    for (const { rel, file } of sources) {
      const visit = (node: ts.Node) => {
        if (ts.isIdentifier(node) && forbidden.has(node.text)) {
          const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
          violations.push(`${rel}:${line + 1} references ${node.text}`);
        } else if (
          (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) &&
          (node.text.includes("registerTools") || node.text.includes("modelContext"))
        ) {
          const { line } = file.getLineAndCharacterOfPosition(node.getStart(file));
          violations.push(`${rel}:${line + 1} contains literal "${node.text}"`);
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }
    expect(violations).toEqual([]);
  });

  it("pins exact @ui-sdk versions and removes retired Radix Select/Dialog/DropdownMenu in package.json", () => {
    const deps = studioPkg.dependencies as Record<string, string>;
    expect(deps["@ui-sdk/core"]).toBe("0.1.0-beta.17");
    expect(deps["@ui-sdk/react"]).toBe("0.1.0-beta.17");
    expect(deps["@ui-sdk/core"]).not.toMatch(/^[~^]/);
    expect(deps["@ui-sdk/react"]).not.toMatch(/^[~^]/);
    expect(deps["@radix-ui/react-select"]).toBeUndefined();
    expect(deps["@radix-ui/react-dialog"]).toBeUndefined();
    expect(deps["@radix-ui/react-dropdown-menu"]).toBeUndefined();
  });
});
