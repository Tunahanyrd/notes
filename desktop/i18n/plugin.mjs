// Runtime Turkish <-> English UI translation.
//
// The frontend source stays in Turkish (the upstream language). This plugin
// rewrites *static* strings in src/ into calls to t() from i18n-runtime, which
// picks the current language at runtime. User data (note text, titles, folder
// names, ...) never passes through the plugin, so it is never translated.
//
// The language can be switched in Settings; the choice is stored in
// localStorage and the webview reloads so module-level strings re-evaluate.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {parse} from '@babel/parser';
import _traverse from '@babel/traverse';
import _generate from '@babel/generator';
import * as types from '@babel/types';

const traverse = _traverse.default ?? _traverse;
const generate = _generate.default ?? _generate;
const dict = JSON.parse(fs.readFileSync(fileURLToPath(new URL('./tr-en.json', import.meta.url)), 'utf8'));

const HELPER = '__notlarT';
const RUNTIME_BASENAME = 'i18n-runtime.ts';
const SRC_ROOT = fileURLToPath(new URL('../src', import.meta.url));

const SKIP_PARENTS = new Set([
  'ImportDeclaration', 'ExportNamedDeclaration', 'ExportAllDeclaration',
  'TSImportType', 'TSExternalModuleReference', 'TSLiteralType',
  'TSEnumMember', 'TSPropertySignature', 'TSMethodSignature',
]);

const has = value => typeof value === 'string' && Object.prototype.hasOwnProperty.call(dict, value);

function isDirective(path) {
  const parent = path.parent;
  return parent?.type === 'ExpressionStatement' && typeof parent.directive === 'string';
}

function isPropertyKey(path) {
  const parent = path.parent;
  return (parent?.type === 'ObjectProperty' || parent?.type === 'ObjectMethod' ||
    parent?.type === 'ClassProperty' || parent?.type === 'ClassMethod') &&
    parent.key === path.node && !parent.computed;
}

function isHelperKey(path) {
  const parent = path.parent;
  return parent?.type === 'CallExpression' &&
    parent.callee?.type === 'Identifier' && parent.callee.name === HELPER &&
    parent.arguments[0] === path.node;
}

/** t("literal") */
function callT(value, args = []) {
  return types.callExpression(types.identifier(HELPER), [
    types.stringLiteral(value),
    ...args,
  ]);
}

function templateKey(node) {
  let key = '';
  node.quasis.forEach((quasi, index) => {
    key += quasi.value.cooked ?? quasi.value.raw;
    if (index < node.expressions.length) key += `{${index}}`;
  });
  return key;
}

/** Keep the single space JSX would render for inline whitespace. */
function jsxSpace(raw, side) {
  if (side === 'start') return /^[ \t]+/.test(raw) ? ' ' : '';
  return /(^|[^\n])[ \t]+$/.test(raw) ? ' ' : '';
}

export function translateUi() {
  return {
    name: 'notlar-ui-i18n',
    enforce: 'pre',
    transform(code, id) {
      if (!/[\\/]src[\\/].*\.(tsx|ts)$/.test(id) || id.endsWith('.d.ts')) return null;
      if (path.basename(id) === RUNTIME_BASENAME) return null;

      const ast = parse(code, {
        sourceType: 'module',
        plugins: ['typescript', 'jsx'],
        errorRecovery: true,
      });
      let changed = false;

      traverse(ast, {
        JSXText(path) {
          const raw = path.node.value;
          const text = raw.replace(/\s+/g, ' ').trim();
          if (!text || !has(text)) return;
          const before = jsxSpace(raw, 'start');
          const after = jsxSpace(raw, 'end');
          let expression = callT(text);
          if (before) expression = types.binaryExpression('+', types.stringLiteral(before), expression);
          if (after) expression = types.binaryExpression('+', expression, types.stringLiteral(after));
          path.replaceWith(types.jsxExpressionContainer(expression));
          changed = true;
        },

        StringLiteral(path) {
          if (SKIP_PARENTS.has(path.parent?.type) || isDirective(path) || isPropertyKey(path) || isHelperKey(path)) return;
          if (!has(path.node.value)) return;
          const expression = callT(path.node.value);
          if (path.parent?.type === 'JSXAttribute') {
            path.replaceWith(types.jsxExpressionContainer(expression));
          } else {
            path.replaceWith(expression);
          }
          changed = true;
        },

        TemplateLiteral(path) {
          const {quasis, expressions} = path.node;
          const key = templateKey(path.node);

          // Word order differs between the languages: use a placeholder entry
          // and pass the expressions through.
          if (has(key)) {
            path.replaceWith(callT(key, expressions));
            changed = true;
            return;
          }

          // Otherwise translate the static chunks individually, keeping the
          // expression order the same.
          const translated = quasis.map(quasi => {
            const value = quasi.value.cooked ?? quasi.value.raw;
            return has(value) ? dict[value] : null;
          });
          if (!translated.some(value => value !== null)) return;

          const parts = [types.stringLiteral('')];
          quasis.forEach((quasi, index) => {
            const value = quasi.value.cooked ?? quasi.value.raw;
            if (value !== '') {
              parts.push(translated[index] !== null ? callT(value) : types.stringLiteral(value));
            }
            if (index < expressions.length) parts.push(expressions[index]);
          });
          let expression = parts[0];
          for (const part of parts.slice(1)) expression = types.binaryExpression('+', expression, part);
          path.replaceWith(expression);
          changed = true;
        },
      });

      if (!changed) return null;

      const from = path.dirname(id);
      const runtime = path.join(SRC_ROOT, RUNTIME_BASENAME);
      let relative = path.relative(from, runtime).replace(/\\/g, '/').replace(/\.ts$/, '');
      if (!relative.startsWith('.')) relative = `./${relative}`;

      const alreadyImported = ast.program.body.some(node =>
        node.type === 'ImportDeclaration' &&
        node.specifiers.some(specifier => specifier.local?.name === HELPER));
      if (!alreadyImported) {
        ast.program.body.unshift(types.importDeclaration(
          [types.importSpecifier(types.identifier(HELPER), types.identifier('t'))],
          types.stringLiteral(relative),
        ));
      }

      const out = generate(ast, {comments: true, sourceMaps: true, sourceFileName: id}, code);
      return {code: out.code, map: out.map};
    },
  };
}
