// Execute frozen AST fragments and React server rendering only; no browser/listener.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { baseline, requireUI } from './env.mjs';
const ts = requireUI('typescript');
const compile = source => ts.transpileModule(source, { compilerOptions: {
  module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
} }).outputText;
function parse(name, source) { return ts.createSourceFile(name, source, ts.ScriptTarget.Latest, true); }
function find(ast, predicate) {
  const nodes = [];
  function visit(node) { if (predicate(node)) nodes.push(node); ts.forEachChild(node, visit); }
  visit(ast); assert.equal(nodes.length, 1, 'exactly one frozen source fragment'); return nodes[0];
}
function variable(ast, name) {
  return find(ast, node => ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === name);
}
// Compile as a body: transpile output is statements, not a JS expression.
function evaluate(source, bindings) {
  bindings = { exports: {}, ...bindings };
  return new Function(...Object.keys(bindings), compile(`const result = (${source});`) + '\nreturn result;')(...Object.values(bindings));
}
export async function proveFilesBaselineStale(clickButton, entry) {
  const selectionSource = await readFile(resolve(baseline, 'src/components/app-shell/appShellSelection.ts'), 'utf8');
  const selectionAST = parse('selection.ts', selectionSource);
  const selectionCode = ['isGeneralProject', 'chooseDefaultProject'].map(name =>
    find(selectionAST, node => ts.isFunctionDeclaration(node) && node.name?.text === name).getText(selectionAST)).join('\n');
  const selection = {};
  new Function('exports', compile(selectionCode))(selection);
  const projects = [{ name: 'seed', displayName: 'e2e-project' }, { name: 'general', kind: 'general' }];
  const selectedProject = selection.chooseDefaultProject(projects);
  assert.equal(selectedProject.name, 'general');
  assert.equal(selection.chooseDefaultProject([...projects].reverse()).name, 'general');
  const area = parse('MainAreaV2.tsx', await readFile(resolve(baseline, 'src/components/app-shell/MainAreaV2.tsx'), 'utf8'));
  const composer = parse('ComposerV2.tsx', await readFile(resolve(baseline, 'src/components/chat-v2/ComposerV2.tsx'), 'utf8'));
  const addButton = find(composer, node => ts.isJsxElement(node) && node.openingElement.tagName.getText(composer) === 'button'
    && node.openingElement.attributes.properties.some(prop => prop.name?.getText(composer) === 'ref' && prop.getText(composer).includes('addTriggerRef')));
  const addClick = find(addButton, node => ts.isJsxAttribute(node) && node.name.getText(composer) === 'onClick');
  const main = parse('MainContent.tsx', await readFile(resolve(baseline, 'src/components/main-content/view/MainContent.tsx'), 'utf8'));
  const filesEntry = find(area, node => ts.isConditionalExpression(node) && node.condition.getText(area) === 'projectFilesEnabled');
  const nav = find(main, node => ts.isJsxElement(node) && node.openingElement.tagName.getText(main) === 'nav'
    && node.getText(main).includes('filesWorkbench.panelControls'));
  const onClick = find(filesEntry, node => ts.isJsxAttribute(node) && node.name.getText(area) === 'onClick');
  const { renderToStaticMarkup } = requireUI('react-dom/server');
  const labels = html => [...html.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].map(match =>
    /aria-label="([^"]*)"/.exec(match[1])?.[1] ?? match[2].replace(/<[^>]*>/g, '').trim());
  const scenarios = [];
  for (const language of ['en', 'zh-CN']) {
    const common = JSON.parse(await readFile(resolve(baseline, `src/i18n/locales/${language}/common.json`), 'utf8'));
    const t = (key, options = {}) => key.split('.').reduce((v, part) => v?.[part], common) ?? options.defaultValue ?? key;
    // Same guard as baseline-stale-proof: the evaluated frozen snippet may only
    // load what it is known to load, so no package bypasses the node_modules gate.
    const guardedRequire = name => {
      assert(name === 'react/jsx-runtime', `Unexpected files dependency ${name}`);
      return requireUI(name);
    };
    const shared = { require: guardedRequire, cn: (...v) => v.filter(Boolean).join(' '), t, FILES_TAB: { labelKey: 'tabs.files' },
      setDashboardMenuOpen() {}, chatHistorySearch: { closeSearch() {} }, setActiveTab() {},
      singlePanelMode: false, explorerVisible: false, explorerCollapsed: true, assistantCollapsed: false,
      assistantVisible: true, toggleExplorer() {}, toggleAssistant() {} };
    const enabled = project => evaluate(variable(area, 'projectFilesEnabled').initializer.getText(area), {
      selectedProject: project, generalConversation: Boolean(project && selection.isGeneralProject(project)),
    });
    const generalHtml = renderToStaticMarkup(evaluate(filesEntry.getText(area), { ...shared,
      projectFilesEnabled: enabled(selectedProject), displayActiveTab: 'chat' }));
    assert.equal(generalHtml, '');
    // General still has a composer button matching the helper's broad regex.
    const chat = JSON.parse(await readFile(resolve(baseline, `src/i18n/locales/${language}/chat.json`), 'utf8'));
    const chatT = key => key.split('.').reduce((v, part) => v?.[part], chat) ?? key;
    let menuOpen = false;
    const composerBindings = { ...shared, t: chatT, addTriggerRef: null, isAddMenuOpen: false,
      setIsAddMenuOpen: update => { menuOpen = update(menuOpen); }, Plus: () => null };
    const composerHtml = renderToStaticMarkup(evaluate(addButton.getText(composer), composerBindings));
    let pattern, clickedLabel;
    await clickButton({ getByRole(role, options) {
      assert.equal(role, 'button'); pattern = options.name;
      const candidates = labels(generalHtml + composerHtml).filter(name => pattern.test(name));
      assert.equal(candidates.length, 1);
      clickedLabel = candidates[0];
      return { first: () => ({ click: async () => evaluate(addClick.initializer.expression.getText(composer), composerBindings)() }) };
    } }, entry.selectorArguments);
    assert.equal(menuOpen, true);
    assert.equal(entry.failure.locator, "locator('body')");
    // Counterfactual registered regular project: original click chooses mode before rail.
    let activeTab = 'chat';
    const topHtml = renderToStaticMarkup(evaluate(filesEntry.getText(area), { ...shared,
      projectFilesEnabled: enabled(projects[0]), displayActiveTab: activeTab }));
    const topClick = () => evaluate(onClick.initializer.expression.getText(area), {
      ...shared, displayActiveTab: activeTab, setActiveTab: tab => { activeTab = tab; },
    })();
    const railHtml = renderToStaticMarkup(evaluate(nav.getText(main), shared));
    const orderedLabels = [...labels(topHtml), ...labels(railHtml)];
    assert(pattern.test(orderedLabels[0]));
    assert(pattern.test(orderedLabels[1]));
    await clickButton({ getByRole(role, options) {
      assert.equal(role, 'button');
      const candidates = orderedLabels.filter(name => options.name.test(name));
      assert.equal(candidates[0], labels(topHtml)[0]);
      return { first: () => ({ click: async () => topClick() }) };
    } }, entry.selectorArguments);
    assert.equal(activeTab, 'files');
    const state = find(main, node => ts.isVariableDeclaration(node)
      && node.name.getText(main) === '[explorerCollapsed, setExplorerCollapsed]');
    let collapsed = evaluate(state.initializer.getText(main), { useState: value => [value, () => {}] })[0];
    assert.equal(collapsed, true);
    const reset = find(main, node => ts.isCallExpression(node) && node.expression.getText(main) === 'useEffect'
      && node.arguments[0]?.getText(main).includes('setExplorerCollapsed(true)'));
    collapsed = false;
    evaluate(reset.arguments[0].getText(main), { isFiles: true,
      setExplorerCollapsed: value => { collapsed = value; }, setAssistantCollapsed() {}, setAssistantOverlayOpen() {} })();
    assert.equal(collapsed, true);
    const visible = c => evaluate(variable(main, 'explorerVisible').initializer.getText(main), {
      isFiles: true, showChat: true, editorExpanded: false, isMobile: false,
      explorerCollapsed: c, singlePanelMode: false, assistantCollapsed: false,
      activeDockPanel: 'assistant', isNarrowWorkbench: false, assistantOverlayOpen: false,
    });
    assert.equal(visible(collapsed), false);
    assert.equal(visible(false), true, 'positive control: opening explorer permits mount');
    const treeGuard = find(main, node => ts.isConditionalExpression(node)
      && node.condition.getText(main) === 'isFiles && explorerVisible'
      && node.whenTrue.getText(main).includes('<FilesV2'));
    assert.equal(treeGuard.whenFalse.getText(main), 'null');
    scenarios.push({ language, defaultProject: selectedProject.name, generalFilesButtonMounted: false,
      generalHelperSelectedButton: clickedLabel, generalAddContextMenuOpen: menuOpen,
      originalBodyAssertionFailure: entry.failure, orderedButtonLabels: orderedLabels, selectedButtonIndex: 0,
      regularProjectAfterOriginalClick: activeTab, railClicked: false, explorerInitialCollapsed: true,
      collapsedAfterEnterFiles: collapsed, explorerVisible: visible(collapsed),
      filesV2Mounted: false, mountGuard: treeGuard.condition.getText(main), openedPositiveControlMounted: visible(false) });
    topClick(); assert.equal(activeTab, 'chat');
    scenarios.at(-1).afterSecondTopModeClick = activeTab;
  }
  return { file: entry.file, title: entry.title, defaultRoute: '/', scenarios };
}
