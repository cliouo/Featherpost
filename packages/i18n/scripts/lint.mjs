#!/usr/bin/env node
/**
 * i18n:lint —— 默认前端界面消息的构建前守门（ADR-0048 决策 8）。
 *
 * 检查：
 *   1. 每个命名空间文件在每种语言下都存在，值只能是字符串，key 按字典序排列且不重复；
 *   2. key 是合法标识符，前缀与所在命名空间文件一致（`mailbox_*` 只能在 `mailbox.json` 里）；
 *   3. 各语言 key 集合与基础语言一致，同一 key 的占位符集合一致；
 *   4. `packages/*\/src` 中通过 `@featherpost/i18n/messages` 导入的 `m.<key>` 都存在，且不用 `m["..."]`；
 *   5. `packages/*\/src` 的字符串、模板与 JSX 文本中不得出现中文（注释不算）。迁移期未清理的文件
 *      列在 `cjk-allowlist.txt`；白名单只许缩短：已清理或已删除的文件仍留在单上同样报错。
 *      不属于界面文案的中文（如验证码识别关键词）在该语句前加注释 `i18n-lint-allow-cjk: 理由`。
 *
 * 用法：
 *   node scripts/lint.mjs                    # 检查，失败时退出码 1
 *   node scripts/lint.mjs --sort             # 把消息文件按 key 重新排序后再检查
 *   node scripts/lint.mjs --write-allowlist  # 用当前违规文件重写白名单（只用于生成基线或删减条目）
 *
 * 只依赖 Node 与本包已有的 typescript（Paraglide 生成 .d.ts 本就需要它），用它的解析器区分注释与字面量。
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(packageDir, "../..");
const packagesDir = path.join(repoRoot, "packages");
const allowlistPath = path.join(packageDir, "cjk-allowlist.txt");
const MESSAGES_MODULE = "@featherpost/i18n/messages";
const ALLOW_PRAGMA = "i18n-lint-allow-cjk";
// 中日韩统一表意文字（含扩展 A 与兼容区）、CJK 标点、全角形式。
const CJK = /[　-〿㐀-䶿一-鿿豈-﫿＀-￯]/;
const PLACEHOLDER = /\{\s*([A-Za-z_$][\w$]*)\s*\}/g;
const KEY = /^[a-z][a-z0-9]*(?:_[a-z0-9]+)+$/;

const args = new Set(process.argv.slice(2));
const errors = [];
const rel = (file) => path.relative(repoRoot, file).split(path.sep).join("/");

// ---------- 消息文件 ----------

const settings = JSON.parse(
  fs.readFileSync(path.join(packageDir, "project.inlang/settings.json"), "utf8"),
);
const { baseLocale, locales } = settings;
const patterns = [].concat(settings["plugin.inlang.messageFormat"]?.pathPattern ?? []);
const namespaces = [];
for (const pattern of patterns) {
  const match = /^\.\/messages\/\{locale\}\/([a-z][a-z0-9]*)\.json$/.exec(pattern);
  if (match) namespaces.push(match[1]);
  else
    errors.push(
      `project.inlang/settings.json: pathPattern「${pattern}」应形如 ./messages/{locale}/<命名空间>.json`,
    );
}
if (!locales.includes(baseLocale)) errors.push(`基础语言 ${baseLocale} 不在 locales 中`);

/** @type {Map<string, Map<string, Map<string, string>>>} 命名空间 → 语言 → key → 文案 */
const catalog = new Map();
const allKeys = new Set();

for (const namespace of namespaces) {
  const byLocale = new Map();
  catalog.set(namespace, byLocale);
  for (const locale of locales) {
    const file = path.join(packageDir, "messages", locale, `${namespace}.json`);
    const where = rel(file);
    if (!fs.existsSync(file)) {
      errors.push(`${where}: 缺少文件（每种语言都要有全部命名空间，内容可以只有 $schema）`);
      continue;
    }
    const text = fs.readFileSync(file, "utf8");
    let json;
    try {
      json = JSON.parse(text);
    } catch (error) {
      errors.push(`${where}: JSON 解析失败：${error.message}`);
      continue;
    }
    if (json === null || typeof json !== "object" || Array.isArray(json)) {
      errors.push(`${where}: 顶层必须是对象`);
      continue;
    }
    const seen = new Set();
    for (const [, key] of text.matchAll(/^\s*"((?:[^"\\]|\\.)*)"\s*:/gm)) {
      if (seen.has(key)) errors.push(`${where}: key「${key}」重复`);
      seen.add(key);
    }
    if (args.has("--sort")) {
      const sorted = Object.fromEntries(
        Object.entries(json).sort(([a], [b]) =>
          a === "$schema" ? -1 : b === "$schema" ? 1 : a < b ? -1 : a > b ? 1 : 0,
        ),
      );
      const next = `${JSON.stringify(sorted, null, 2)}\n`;
      if (next !== text) fs.writeFileSync(file, next);
      json = sorted;
    }
    const messages = new Map();
    let previous = "";
    for (const [key, value] of Object.entries(json)) {
      if (key === "$schema") continue;
      if (!KEY.test(key))
        errors.push(`${where}: key「${key}」不是合法标识符（小写字母、数字、下划线，含模块前缀）`);
      if (!key.startsWith(`${namespace}_`))
        errors.push(`${where}: key「${key}」应以「${namespace}_」开头（命名空间与文件名一致）`);
      if (typeof value !== "string")
        errors.push(`${where}: key「${key}」的值必须是字符串（复数等用 Intl.PluralRules 在代码里选消息）`);
      if (key < previous)
        errors.push(`${where}: key「${key}」未按字典序排列（可运行 pnpm i18n:lint --sort）`);
      previous = key;
      messages.set(key, typeof value === "string" ? value : "");
      allKeys.add(key);
    }
    byLocale.set(locale, messages);
  }
}

const placeholders = (text) =>
  [...new Set([...text.matchAll(PLACEHOLDER)].map((match) => match[1]))].sort().join(", ");

for (const [namespace, byLocale] of catalog) {
  const base = byLocale.get(baseLocale);
  if (!base) continue;
  for (const locale of locales) {
    if (locale === baseLocale) continue;
    const other = byLocale.get(locale);
    if (!other) continue;
    const where = `messages/${locale}/${namespace}.json`;
    for (const key of base.keys())
      if (!other.has(key)) errors.push(`${where}: 缺少 key「${key}」（${baseLocale} 中有）`);
    for (const key of other.keys())
      if (!base.has(key)) errors.push(`${where}: 多出 key「${key}」（${baseLocale} 中没有）`);
    for (const [key, text] of base) {
      if (!other.has(key)) continue;
      const expected = placeholders(text);
      const actual = placeholders(other.get(key));
      if (expected !== actual)
        errors.push(
          `${where}: key「${key}」的占位符 {${actual}} 与 ${baseLocale} 的 {${expected}} 不一致`,
        );
    }
  }
}

// ---------- 源码 ----------

function sourceFiles(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "paraglide") continue;
      sourceFiles(full, out);
    } else if (/\.(ts|tsx)$/.test(entry.name) && !entry.name.endsWith(".d.ts")) {
      out.push(full);
    }
  }
  return out;
}

const files = [];
for (const entry of fs.readdirSync(packagesDir, { withFileTypes: true })) {
  const src = path.join(packagesDir, entry.name, "src");
  if (entry.isDirectory() && fs.existsSync(src)) sourceFiles(src, files);
}
files.sort();

function hasAllowPragma(node, text) {
  const ranges = ts.getLeadingCommentRanges(text, node.getFullStart()) ?? [];
  return ranges.some((range) => text.slice(range.pos, range.end).includes(ALLOW_PRAGMA));
}

/** @type {Map<string, string[]>} 文件 → 含中文字面量的位置 */
const offenders = new Map();

for (const file of files) {
  const text = fs.readFileSync(file, "utf8");
  const source = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  const where = rel(file);
  const messageNames = new Set();
  for (const statement of source.statements) {
    if (
      ts.isImportDeclaration(statement) &&
      ts.isStringLiteral(statement.moduleSpecifier) &&
      statement.moduleSpecifier.text === MESSAGES_MODULE
    ) {
      const bindings = statement.importClause?.namedBindings;
      if (bindings && ts.isNamedImports(bindings))
        for (const element of bindings.elements)
          if ((element.propertyName ?? element.name).text === "m")
            messageNames.add(element.name.text);
    }
  }
  const hits = [];
  const position = (node) => {
    const { line, character } = source.getLineAndCharacterOfPosition(node.getStart(source));
    return `${line + 1}:${character + 1}`;
  };
  const visit = (node) => {
    if (hasAllowPragma(node, text)) return;
    if (
      ts.isPropertyAccessExpression(node) &&
      ts.isIdentifier(node.expression) &&
      messageNames.has(node.expression.text) &&
      !allKeys.has(node.name.text)
    )
      errors.push(`${where}:${position(node)}: 引用了不存在的消息 m.${node.name.text}`);
    if (
      ts.isElementAccessExpression(node) &&
      ts.isIdentifier(node.expression) &&
      messageNames.has(node.expression.text)
    )
      errors.push(`${where}:${position(node)}: 消息只用 m.<key>() 调用，不用 m[...]`);
    if (
      (ts.isStringLiteral(node) ||
        ts.isNoSubstitutionTemplateLiteral(node) ||
        ts.isTemplateHead(node) ||
        ts.isTemplateMiddle(node) ||
        ts.isTemplateTail(node) ||
        ts.isRegularExpressionLiteral(node) ||
        ts.isJsxText(node)) &&
      CJK.test(node.getText(source))
    )
      hits.push(position(node));
    ts.forEachChild(node, visit);
  };
  visit(source);
  if (hits.length > 0) offenders.set(where, hits);
}

// ---------- 白名单 ----------

const ALLOWLIST_HEADER = `# i18n:lint 中文字面量白名单（迁移期基线，ADR-0048 决策 8）。
# 每行一个仓库相对路径。只许删减：迁移完一个文件就删掉它这一行；新文件不得加入。
# 文件里已无中文字面量却仍在单上，i18n:lint 同样报错。
`;

function readAllowlist() {
  if (!fs.existsSync(allowlistPath)) return [];
  return fs
    .readFileSync(allowlistPath, "utf8")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));
}

if (args.has("--write-allowlist")) {
  fs.writeFileSync(allowlistPath, ALLOWLIST_HEADER + [...offenders.keys()].sort().join("\n") + "\n");
  console.log(`已写入 ${rel(allowlistPath)}：${offenders.size} 个文件`);
}

const allowlist = readAllowlist();
const allowed = new Set();
for (const entry of allowlist) {
  if (allowed.has(entry)) errors.push(`cjk-allowlist.txt: 「${entry}」重复`);
  allowed.add(entry);
  if (!fs.existsSync(path.join(repoRoot, entry)))
    errors.push(`cjk-allowlist.txt: 「${entry}」已不存在，请删除这一行`);
  else if (!offenders.has(entry))
    errors.push(`cjk-allowlist.txt: 「${entry}」已无中文字面量，请删除这一行`);
}
for (const [file, hits] of offenders) {
  if (allowed.has(file)) continue;
  const shown = hits.slice(0, 5).join(", ") + (hits.length > 5 ? ` 等 ${hits.length} 处` : "");
  errors.push(`${file}: 含中文字面量（${shown}），请改用 m.<key>()`);
}

// ---------- 结果 ----------

if (errors.length > 0) {
  for (const error of errors) console.error(`✗ ${error}`);
  console.error(`\ni18n:lint 失败：${errors.length} 个问题`);
  process.exit(1);
}
console.log(
  `i18n:lint 通过：${namespaces.length} 个命名空间 × ${locales.length} 种语言，${allKeys.size} 个 key；` +
    `扫描 ${files.length} 个源文件，白名单剩 ${allowed.size} 个`,
);
