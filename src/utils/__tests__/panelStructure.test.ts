import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import ts from 'typescript';

/**
 * 「切换面板收起态不得改变组件树结构」的机械化守卫（铁律 20）。
 *
 * 为什么值得有：这条规则被破坏时**没有任何编译错误、没有测试失败，界面也照常工作**——
 * 症状只是"点一下收缩，右区闪两帧"（模板库闪「暂无模板」、项目列闪「暂无项目」、
 * 窄轨闪空白），而且每一帧都短到容易被当成"错觉"。它的成因却很确定：
 * 只要承载带状态子树的那棵树的**根元素类型**随状态变（`cond ? <div>…</div> : <ResizablePanel/>`），
 * React 就会把整棵子树卸载重建——右区里那些 useState（模板、文件树、编辑器实例）
 * 全部回到初值并各自重发一次 IPC。
 *
 * 判据用 TypeScript 解析器而不是 grep：要断言的正是**组件树的结构**
 * （谁是根、谁在条件表达式里），正则匹配这类嵌套结构必然写错。
 *
 * 它挡的是**复发**，不替代"真跑一次"：本文件只保证结构没被改回原样。
 */

const LAYOUT_DIR = path.resolve(import.meta.dirname, '../../components/layout');

/** 解析成 TSX 语法树（带 parent 指针，供向上追溯用） */
function parse(file: string): ts.SourceFile {
  const full = path.join(LAYOUT_DIR, file);
  return ts.createSourceFile(full, fs.readFileSync(full, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
}

/** 从节点往上找，是否落在"条件渲染"里（三元 / && / `cond ? null : x`） */
function insideConditional(node: ts.Node): boolean {
  let p: ts.Node | undefined = node.parent;
  while (p) {
    if (ts.isJsxExpression(p) && p.expression) {
      if (ts.isConditionalExpression(p.expression)) return true;
      if (ts.isBinaryExpression(p.expression) && p.expression.operatorToken.kind === ts.SyntaxKind.AmpersandAmpersandToken) return true;
    }
    p = p.parent;
  }
  return false;
}

/** 收集整棵树里所有直接渲染 `{标识符}` 的 JSX 表达式节点 */
function slotExpressions(sf: ts.SourceFile, name: string): ts.JsxExpression[] {
  const found: ts.JsxExpression[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isJsxExpression(node) && node.expression && ts.isIdentifier(node.expression) && node.expression.text === name) {
      found.push(node);
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return found;
}

test('ResizablePanel：两侧槽位都是无条件渲染（只有分隔条允许折叠）', () => {
  const sf = parse('ResizablePanel.tsx');
  // 解析器失效的下限断言：结构扫描一旦"扫不到东西"就会静默通过，比没有守卫更危险
  let jsxCount = 0;
  const countJsx = (node: ts.Node) => { if (ts.isJsxElement(node) || ts.isJsxSelfClosingElement(node)) jsxCount++; ts.forEachChild(node, countJsx); };
  countJsx(sf);
  assert.ok(jsxCount >= 4, `只解析出 ${jsxCount} 个 JSX 节点，解析器很可能失效`);

  for (const slot of ['left', 'right']) {
    const exprs = slotExpressions(sf, slot);
    assert.equal(exprs.length, 1, `ResizablePanel 里 {${slot}} 应恰好出现一次，实际 ${exprs.length} 次`);
    assert.ok(
      !insideConditional(exprs[0]),
      `ResizablePanel 的 {${slot}} 槽位不能放进条件渲染：收起态只允许换左右槽的**尺寸**与分隔条，`
      + '一旦这个槽整体进条件，它在收起/展开时就会被卸载重建，子树里的状态（模板、文件树、编辑器）全丢',
    );
  }
});

test('MainLayout：主内容的根节点不随收起态变化', () => {
  const sf = parse('MainLayout.tsx');

  let mainContent: ts.Expression | undefined;
  const visit = (node: ts.Node) => {
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === 'mainContent') {
      mainContent = node.initializer;
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);

  assert.ok(mainContent, 'MainLayout.tsx 里找不到 mainContent —— 变量被改名/删除，这条守卫已失效，请同步更新');
  // 语法树里 `( <X/> )` 的括号是一个真实的 ParenthesizedExpression 节点（TS 不像 Babel 那样吞掉它）
  let init = mainContent as ts.Expression;
  while (ts.isParenthesizedExpression(init)) init = init.expression;
  // 自闭合的 `<X/>` 是 JsxSelfClosingElement、不是 JsxElement——两种都要认（本组件正是自闭合写法）
  assert.ok(
    ts.isJsxElement(init) || ts.isJsxSelfClosingElement(init),
    'mainContent 必须是**单个** JSX 元素（无条件）。写成 `cond ? <div>…</div> : <ResizablePanel/>` '
    + '会让根元素类型随状态变化，React 因而卸载重建整棵右区子树——收缩一次闪两帧，'
    + '详情页/模板库/文件树全部重挂载并重发 IPC。收起态请走 ResizablePanel 的 collapsed 属性',
  );
  const tagName = ts.isJsxElement(init) ? init.openingElement.tagName : (init as ts.JsxSelfClosingElement).tagName;
  assert.equal(
    tagName.getText(sf),
    'ResizablePanel',
    'mainContent 的根必须是 ResizablePanel（两侧槽位与外层 div 由它统一持有，位置才稳定）',
  );
});
