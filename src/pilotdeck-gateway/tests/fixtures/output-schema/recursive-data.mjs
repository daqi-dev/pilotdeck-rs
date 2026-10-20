// One cold, default-stack SDK validation per process. Keep validation at
// module scope: extra JS call frames move V8's native stack boundary.
// The generator binds this import to the hash-checked baseline provider
// in a temporary copy; a dynamic import changes the cold module call stack.
import {AjvJsonSchemaValidator} from 'pdrs:baseline-ajv-provider';
const key = process.argv[2];
const depth = Number(process.argv[3]);
if (!['a', 'next'].includes(key) || !Number.isSafeInteger(depth) || depth < 0) {
  throw new Error('expected a|next and a nonnegative integer depth');
}
const schema = {type: 'object', properties: {[key]: {$ref: '#'}}};
let data = {};
let cursor = data;
for (let index = 0; index < depth; index++) {
  cursor[key] = {};
  cursor = cursor[key];
}
const validate = new AjvJsonSchemaValidator().getValidator(schema);
try {
  const result = validate(data);
  console.log(JSON.stringify(result.valid ? {} : {errorText: result.errorMessage}));
} catch (error) {
  if (!(error instanceof RangeError)) throw error;
  console.log(JSON.stringify({validationThrow: {name: error.name, message: error.message}}));
}
