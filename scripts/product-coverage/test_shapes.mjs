import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { assertRouterShapes } from './enumerate.mjs';
const require = createRequire(import.meta.url);
const ts = require('../gateway-parity/node_modules/typescript');
const parse = text => ts.createSourceFile('fixture.js', text, ts.ScriptTarget.Latest, true);
for (const text of [
  "const myRouter = express.Router(); myRouter.get('/x', handler);",
  "const router = express.Router(); const alias = router; alias.post('/x', handler);",
  "const router = express.Router(); router.route(dynamicPath()).get(handler);",
]) assert.throws(() => assertRouterShapes(parse(text)));
assert.doesNotThrow(() => assertRouterShapes(parse("const map = new Map(); map.get('key'); const router = express.Router(); router.route('/x').get(handler);")));
console.log('Express receiver/alias/dynamic-chain negative controls: PASS');
