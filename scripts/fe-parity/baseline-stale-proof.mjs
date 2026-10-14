// Reproducible source execution only: no browser, process spawn, or listener.
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { proveFilesBaselineStale } from './files-stale-proof.mjs';
import { baseline, requireUI } from './env.mjs';

const ts = requireUI('typescript');
const transpile = source => ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
const cn = (...values) => values.filter(Boolean).join(' ');
const jsxRequire = name => {
  assert.equal(name, 'react/jsx-runtime');
  return requireUI(name);
};
const translate = locale => (key, options = {}) => key.split('.').reduce((row, part) => row?.[part], locale) ?? options.defaultValue ?? key;
function buttonLabels(html) {
  return [...html.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].map((match) =>
    /aria-label="([^"]*)"/.exec(match[1])?.[1] ?? match[2].replace(/<[^>]*>/g, '').trim());
}
async function sourceFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  return (await Promise.all(entries.map(entry => entry.isDirectory() ? sourceFiles(resolve(directory, entry.name))
    : /\.[cm]?[jt]sx?$/.test(entry.name) ? [resolve(directory, entry.name)] : []))).flat();
}
export async function proveBaselineStale(catalog) {
  const fixtureSource = await readFile(resolve(baseline, 'e2e/real/fixtures.mjs'), 'utf8');
  const fixtureAST = ts.createSourceFile('fixtures.mjs', fixtureSource, ts.ScriptTarget.Latest, true);
  const statement = fixtureAST.statements.find(node => ts.isVariableStatement(node)
    && node.declarationList.declarations.some(declaration => declaration.name.text === 'clickButton'));
  assert(statement, 'frozen clickButton declaration');
  const helper = {};
  new Function('exports', 'expect', transpile(statement.getText(fixtureAST)))(helper,
    () => ({ toBeVisible: async () => {}, toBeEnabled: async () => {} }));

  const sidebarSource = await readFile(resolve(baseline, 'src/components/settings/view/SettingsSidebar.tsx'), 'utf8');
  const skillSource = await readFile(resolve(baseline, 'src/components/main-content-v2/SkillsV2.tsx'), 'utf8');
  const skillAST = ts.createSourceFile('SkillsV2.tsx', skillSource, ts.ScriptTarget.Latest, true);
  const header = skillAST.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'Header');
  assert(header);
  const { renderToStaticMarkup } = requireUI('react-dom/server');
  const { createElement } = requireUI('react');
  const labels = { settings: {}, skills: {} };
  for (const language of ['en', 'zh-CN']) {
    const settings = JSON.parse(await readFile(resolve(baseline, `src/i18n/locales/${language}/settings.json`), 'utf8'));
    const common = JSON.parse(await readFile(resolve(baseline, `src/i18n/locales/${language}/common.json`), 'utf8'));
    const module = {};
    const dependencies = {
      'react/jsx-runtime': requireUI('react/jsx-runtime'),
      'react-i18next': { useTranslation: () => ({ t: translate(settings) }) },
      '../../../lib/utils.js': { cn },
      '../../../assets/pilotdeck-wordmark-dark.png': 'logo-dark',
      '../../../assets/pilotdeck-wordmark-light.png': 'logo-light',
      './navIcons': { SETTINGS_BACK_ICON: '', SETTINGS_NAV_ICONS: {} },
    };
    new Function('exports', 'require', transpile(sidebarSource))(module, name => {
      assert(Object.hasOwn(dependencies, name), `Unexpected sidebar dependency ${name}`);
      return dependencies[name];
    });
    labels.settings[language] = buttonLabels(renderToStaticMarkup(createElement(module.default,
      { selectedKey: 'general', onSelect() {}, onClose() {} })));
    assert.deepEqual(labels.settings[language], [settings.settingsPage.backToProjects]);
    const skillHeader = new Function('exports', 'require', 'cn', 'Sparkles', 'RefreshCw', 'Plus',
      transpile(header.getText(skillAST)) + '\nreturn Header;')({}, jsxRequire, cn, () => null, () => null, () => null);
    labels.skills[language] = buttonLabels(renderToStaticMarkup(createElement(skillHeader,
      { cwd: '/isolated/e2e-project', generalCwd: false, loading: false, onRefresh() {}, onNew() {}, compact: false, t: translate(common) })));
    assert(labels.skills[language].includes(common.skillsTab.newSkill));
  }
  const references = [];
  for (const file of await sourceFiles(resolve(baseline, 'src'))) {
    if ((await readFile(file, 'utf8')).includes('backToApp')) references.push(file);
  }
  assert.deepEqual(references, [], 'backToApp has no executable source references');
  const proofs = [];
  for (const row of catalog.tests) {
    if (row.file === 'real/files.spec.mjs') {
      proofs.push(await proveFilesBaselineStale(helper.clickButton, row));
      continue;
    }
    let pattern;
    await helper.clickButton({ getByRole(role, options) {
      assert.equal(role, 'button'); pattern = options.name;
      return { first: () => ({ click: async () => {} }) };
    } }, row.selectorArguments);
    const actual = labels[row.file === 'real/skills.spec.mjs' ? 'skills' : 'settings'];
    for (const names of Object.values(actual)) assert(names.every(name => !pattern.test(name)), `${row.title}: actual button cannot match`);
    assert.equal(row.failure.locator, `getByRole('button', { name: ${pattern} }).first()`);
    proofs.push({ file: row.file, title: row.title, pattern: pattern.source, actualButtonLabels: actual, matched: false });
  }
  return { status: 'PASS', scope: 'OFFLINE_SOURCE_EXECUTION_ONLY', frozenHelperExecuted: true,
    frozenComponentsRendered: true, executableBackToAppReferences: references, proofs };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const { BASELINE_STALE, validateBaselineStale } = await import('./baseline-stale.mjs');
  await validateBaselineStale();
  console.log(JSON.stringify(await proveBaselineStale(BASELINE_STALE), null, 2));
}
